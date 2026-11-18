//! Machine code between compiled functions, the run time and the host.

use bun_engine_asm::prelude::*;
use bun_engine_asm::{Bank, Features, Options, Reg};
use bun_engine_exec_memory::{CodeHandle, OutOfExecutableMemory, Priority, link};
use bun_engine_wasm_compiler::abi::{
    ARGUMENT_FPRS, ARGUMENT_GPRS, Callable, FunctionSlot, HostLayout, INSTANCE, InstanceLayout,
    MEMORY_BASE, RESULT_FPRS, RESULT_GPRS, SLOT, STUB_CALLEE, Stubs, Trap, frame,
};

use crate::call::{self, Registers};
use crate::code;
use crate::frames::{self, Handler};
use crate::instance::Instance;
use crate::operations::{self, Conduct};
use crate::outcome::Failure;

const FP: Gpr = abi::FRAME_POINTER;
const SP: Gpr = abi::STACK_POINTER;
const ARGUMENT: [Gpr; 4] = *abi::ARGUMENT_GPRS.first_chunk().unwrap();
const RESULT: Gpr = abi::RETURN_GPRS[0];
const FAILURE: Gpr = abi::RETURN_GPRS[1];
/// Neither an argument, nor a result, nor [`SLOT`].
const TEMP: Gpr = abi::NON_ARGUMENT_GPRS[1];
const HEADER_SIZE: i32 = frame::ARGUMENTS - frame::INSTANCE;

#[cfg(target_arch = "x86_64")]
const NOT_KEPT: RegisterSet = RegisterSet::EMPTY;
#[cfg(target_arch = "aarch64")]
const NOT_KEPT: RegisterSet =
    RegisterSet::of(&[abi::SCRATCH_GPRS[0], bun_engine_asm::arm64::LR], &[]);
/// What a stub saves so as to keep all registers.
const CHANGED_BY_CALLS: RegisterSet = abi::CALLER_SAVED
    .union(abi::CALLEE_SAVED_LOW_HALF)
    .difference(NOT_KEPT);

pub(crate) struct StubCode {
    _code: CodeHandle,
    pub(crate) for_compilers: Stubs,
    pub(crate) enter: usize,
    pub(crate) host_call: usize,
    pub(crate) compile_lazily: usize,
    pub(crate) resume: usize,
    /// Takes a [`Failure`] in the second argument register and the return address in the third.
    pub(crate) trap: usize,
}

impl StubCode {
    pub(crate) fn new(host: &HostLayout, cpu: Features) -> Result<Self, OutOfExecutableMemory> {
        let mut masm = MacroAssembler::new(Options {
            features: cpu,
            shorten_jumps: true,
            seed: 0,
        });
        let [trap, fail_in_bare_frame, fail] = [(); 3].map(|()| masm.new_label());
        let mut writer = Writer {
            masm,
            host,
            trap,
            fail_in_bare_frame,
            fail,
        };
        let operations = operations::ALL.map(|(conduct, code)| writer.operation(conduct, code));
        let traps: [_; Trap::COUNT] = core::array::from_fn(|index| writer.trap(Trap::ALL[index]));
        writer.raise();
        let resume = writer.resume();
        let enter = writer.enter();
        let host_call = writer.host_call();
        let compile_lazily = writer.compile_lazily();
        let write_barrier = writer.write_barrier();
        let tier_up_at_entry = writer.tier_up(code::tier_up_at_entry as *const (), None);
        let tier_up_in_loop = writer.tier_up(code::tier_up_in_loop as *const (), Some(SLOT));

        let bytes = writer.masm.finish().expect("the stubs are small");
        let code = link(&bytes, Priority::Required, |_| unreachable!())?;
        code.describe(|| "wasm stubs".into());
        let address = |label| code.address(bytes.offset_of(label));
        Ok(Self {
            for_compilers: Stubs {
                operations: operations.map(address),
                traps: traps.map(address),
                write_barrier: address(write_barrier),
                tier_up_at_entry: address(tier_up_at_entry),
                tier_up_in_loop: address(tier_up_in_loop),
            },
            enter: address(enter),
            host_call: address(host_call),
            compile_lazily: address(compile_lazily),
            resume: address(resume),
            trap: address(trap),
            _code: code,
        })
    }
}

struct Writer<'a> {
    masm: MacroAssembler,
    host: &'a HostLayout,
    trap: Label,
    /// As `fail`, when the header of the frame is not filled in.
    fail_in_bare_frame: Label,
    /// Takes a [`Failure`] in [`FAILURE`] and needs [`INSTANCE`].
    fail: Label,
}

fn registers(gprs: &[Gpr], fprs: &[Fpr]) -> impl Iterator<Item = Reg> {
    let gprs = gprs.iter().map(|&reg| Reg::from(reg));
    gprs.chain(fprs.iter().map(|&reg| Reg::from(reg)))
}

impl Writer<'_> {
    fn start(&mut self) -> Label {
        self.masm.align(16, 15);
        self.masm.here()
    }

    fn call_native(&mut self, function: *const ()) {
        self.masm.move64(TEMP, function.expose_provenance());
        self.masm.call_indirect(TEMP);
    }

    fn call_fallible(&mut self, function: *const ()) {
        self.call_native(function);
        self.masm.branch_if_not_zero64(FAILURE, self.fail);
    }

    fn load_runtime_data(&mut self, dst: Gpr) {
        let place = Address::new(INSTANCE, InstanceLayout::runtime_data(self.host));
        self.masm.load64(dst, place);
    }

    fn load_vm(&mut self, dst: Gpr) {
        let place = Address::new(INSTANCE, InstanceLayout::vm(self.host));
        self.masm.load64(dst, place);
    }

    fn load_memory_base(&mut self) {
        let place = Address::new(INSTANCE, InstanceLayout::memory_base(self.host));
        self.masm.load64(MEMORY_BASE, place);
    }

    fn fill_in_header(&mut self) {
        self.masm
            .store64(Address::new(FP, frame::INSTANCE), INSTANCE);
        self.masm
            .store64(Address::new(FP, frame::CALLEE), STUB_CALLEE);
    }

    fn operation(&mut self, conduct: Conduct, function: *const ()) -> Label {
        let start = self.start();
        if conduct == Conduct::Succeeds {
            self.load_runtime_data(ARGUMENT[0]);
            self.masm.move64(TEMP, function.expose_provenance());
            self.masm.jump_indirect(TEMP);
            return start;
        }
        self.masm.enter_frame();
        if conduct == Conduct::CallsHost {
            self.fill_in_header();
            self.load_vm(TEMP);
            self.masm
                .store64(Address::new(TEMP, self.host.vm_top_frame), FP);
        }
        self.load_runtime_data(ARGUMENT[0]);
        self.call_native(function);
        self.masm
            .branch_if_not_zero64(FAILURE, self.fail_in_bare_frame);
        self.masm.leave_frame();
        self.masm.ret();
        start
    }

    fn trap(&mut self, trap: Trap) -> Label {
        let start = self.masm.here();
        #[cfg(target_arch = "x86_64")]
        self.masm.pop(ARGUMENT[2]);
        #[cfg(target_arch = "aarch64")]
        self.masm.move64(ARGUMENT[2], bun_engine_asm::arm64::LR);
        self.masm.move32(ARGUMENT[1], Failure::trap(trap).word());
        self.masm.jump(self.trap);
        start
    }

    fn raise(&mut self) {
        // Nothing goes on in the frame, and the stack pointer may be beyond the limit.
        self.masm.bind(self.trap);
        self.masm
            .load64(INSTANCE, Address::new(FP, frame::INSTANCE));
        self.masm
            .compute_address(SP, Address::new(FP, -HEADER_SIZE));
        self.masm.push_pair(FP, ARGUMENT[2]);
        self.masm.move64(FP, SP);
        self.masm.move64(FAILURE, ARGUMENT[1]);

        self.masm.bind(self.fail_in_bare_frame);
        self.fill_in_header();

        self.masm.bind(self.fail);
        self.masm.move64(ARGUMENT[1], FAILURE);
        self.masm.move64(ARGUMENT[2], FP);
        self.load_runtime_data(ARGUMENT[0]);
        self.call_native(frames::raise as *const ());
        let [fp, address] = abi::RETURN_GPRS;
        self.masm.move64(FP, fp);
        self.load_vm(abi::VM);
        self.masm.load_tags();
        self.masm.jump_indirect(address);
    }

    fn resume(&mut self) -> Label {
        let handler = Address::new(TEMP, Instance::HANDLER);
        let start = self.start();
        self.masm
            .load64(INSTANCE, Address::new(FP, frame::INSTANCE));
        self.load_memory_base();
        self.load_runtime_data(TEMP);
        self.masm
            .load64(RESULT, handler.plus(Handler::STACK_POINTER));
        self.masm.move64(SP, RESULT);
        self.masm.jump_indirect(handler.plus(Handler::ADDRESS));
        start
    }

    fn store_values(&mut self, gprs: &[Gpr], fprs: &[Fpr], area: Address) {
        for reg in registers(gprs, fprs) {
            let place = area.plus(Registers::offset(reg));
            self.masm
                .store_registers(RegisterSet::EMPTY.with(reg), 16, place);
        }
    }

    fn load_values(&mut self, gprs: &[Gpr], fprs: &[Fpr], area: Address) {
        for reg in registers(gprs, fprs) {
            let place = area.plus(Registers::offset(reg));
            self.masm
                .load_registers(RegisterSet::EMPTY.with(reg), 16, place);
        }
    }

    fn enter(&mut self) -> Label {
        let values = Address::new(FP, -Registers::SIZE);
        let callable = Address::new(TEMP, self.host.function_callable);
        let stack_bytes = callable.plus(size_of::<Callable>() as i32);
        // Leaves the function in `TEMP` and the stack pointer for the call in `dst`.
        let find_place = |writer: &mut Self, dst| {
            let masm = &mut writer.masm;
            masm.load64(TEMP, Address::new(FP, frame::CALLEE));
            masm.load64(INSTANCE, callable.plus(Callable::INSTANCE));
            masm.compute_address(dst, values.plus(-HEADER_SIZE));
            masm.sub64(dst, dst, stack_bytes);
        };
        let transfer = |writer: &mut Self, function| {
            let callee_frame = Address::new(SP, frame::CALLEE_FRAME_FROM_STACK_POINTER);
            writer.load_runtime_data(ARGUMENT[0]);
            writer.masm.move64(ARGUMENT[1], FP);
            writer.masm.compute_address(ARGUMENT[2], values);
            writer.masm.compute_address(ARGUMENT[3], callee_frame);
            writer.call_fallible(function);
        };
        let no_stack = self.masm.new_label();

        let start = self.start();
        self.masm.enter_frame();
        find_place(self, ARGUMENT[3]);
        let limit = Address::new(INSTANCE, InstanceLayout::stack_limit(self.host));
        self.masm
            .branch64(Cond::Below, ARGUMENT[3], limit, no_stack);
        self.masm.move64(SP, ARGUMENT[3]);
        transfer(self, call::put_arguments as *const ());
        self.load_values(ARGUMENT_GPRS, ARGUMENT_FPRS, values);
        self.masm.load64(TEMP, Address::new(FP, frame::CALLEE));
        self.masm.load64(SLOT, callable.plus(Callable::SLOT));
        self.load_memory_base();
        self.masm
            .call_indirect(Address::new(SLOT, FunctionSlot::ENTRY));
        find_place(self, ARGUMENT[3]);
        self.masm.move64(SP, ARGUMENT[3]);
        self.store_values(RESULT_GPRS, RESULT_FPRS, values);
        transfer(self, call::take_results as *const ());
        self.load_vm(abi::VM);
        self.masm.load_tags();
        self.masm.leave_frame();
        self.masm.ret();

        self.masm.bind(no_stack);
        let failure = Failure::trap(Trap::StackOverflow);
        self.masm.move32(FAILURE, failure.word());
        self.masm.jump(self.fail);
        start
    }

    fn host_call(&mut self) -> Label {
        let values = Address::new(SP, 0);
        let start = self.start();
        self.masm.enter_frame();
        self.fill_in_header();
        self.masm.sub64(SP, SP, Registers::SIZE);
        self.store_values(ARGUMENT_GPRS, ARGUMENT_FPRS, values);
        self.load_runtime_data(ARGUMENT[0]);
        self.masm.move64(ARGUMENT[1], SLOT);
        self.masm.move64(ARGUMENT[2], SP);
        self.masm.move64(ARGUMENT[3], FP);
        self.call_fallible(call::call_host as *const ());
        self.load_values(RESULT_GPRS, RESULT_FPRS, values);
        self.masm.leave_frame();
        self.masm.ret();
        start
    }

    /// The general registers, and where the others are. Offsets are aligned, so no scratch register is needed.
    fn places_for(saved: RegisterSet) -> [(RegisterSet, Address); 2] {
        let gprs = saved.in_bank(Bank::Gpr);
        let vectors_start = (gprs.len() * 8).next_multiple_of(16) as i32;
        [
            (gprs, Address::new(SP, 0)),
            (saved - gprs, Address::new(SP, vectors_start)),
        ]
    }

    fn enter_frame_saving(&mut self, saved: RegisterSet) -> Label {
        let places @ [_, (vectors, vectors_start)] = Self::places_for(saved);
        let start = self.start();
        self.masm.enter_frame();
        self.masm
            .sub64(SP, SP, vectors_start.offset() + vectors.len() as i32 * 16);
        for (registers, place) in places {
            self.masm.store_registers(registers, 16, place);
        }
        start
    }

    fn leave_frame_restoring(&mut self, saved: RegisterSet) {
        for (registers, place) in Self::places_for(saved) {
            self.masm.load_registers(registers, 16, place);
        }
        self.masm.leave_frame();
    }

    fn compile_lazily(&mut self) -> Label {
        let saved = RegisterSet::of(ARGUMENT_GPRS, ARGUMENT_FPRS).with(SLOT.into());
        let start = self.enter_frame_saving(saved);
        self.fill_in_header();
        self.load_runtime_data(ARGUMENT[0]);
        self.masm.move64(ARGUMENT[1], SLOT);
        self.call_fallible(code::compile_lazily as *const ());
        self.leave_frame_restoring(saved);
        self.masm
            .jump_indirect(Address::new(SLOT, FunctionSlot::ENTRY));
        start
    }

    fn write_barrier(&mut self) -> Label {
        let start = self.enter_frame_saving(CHANGED_BY_CALLS);
        self.load_runtime_data(ARGUMENT[0]);
        self.masm.move64(ARGUMENT[1], SLOT);
        self.call_native(operations::write_barrier as *const ());
        self.leave_frame_restoring(CHANGED_BY_CALLS);
        self.masm.ret();
        start
    }

    /// `function` gets the instance, the frame pointer of the caller, the return address and [`SLOT`].
    fn tier_up(&mut self, function: *const (), result: Option<Gpr>) -> Label {
        let saved = CHANGED_BY_CALLS - result.into_iter().collect();
        let start = self.enter_frame_saving(saved);
        self.load_runtime_data(ARGUMENT[0]);
        self.masm
            .load_pair64(ARGUMENT[1], ARGUMENT[2], Address::new(FP, 0));
        self.masm.move64(ARGUMENT[3], SLOT);
        self.call_native(function);
        if let Some(result) = result {
            self.masm.move64(result, RESULT);
        }
        self.leave_frame_restoring(saved);
        self.masm.ret();
        start
    }
}

const _: () = assert!(Registers::SIZE % 16 == 0);
