//! Runs compiled code: a trampoline that sets the pinned registers and builds a frame, and a stub for the exits.

use bun_engine_asm::prelude::*;
use bun_engine_asm::{Features, MacroAssembler, Options};
use bun_engine_exec_memory::{CodeHandle, Priority, link};
use bun_engine_vm::{CallFrame, Value};

use crate::lower::output::{EXIT_REGISTER, Output, symbol};

/// Words of the buffer that the trampoline uses. It stands in for the `Vm`, whose fields all read zero, so that
/// the stack limit never stops the prologue.
const BUFFER_WORDS: usize = 1 << 13;
const SAVED_STACK: i32 = (BUFFER_WORDS as i32 - 8) * 8;
const SAVED_FRAME: i32 = SAVED_STACK + 8;
const TARGET: i32 = SAVED_STACK + 16;
const EXIT_NUMBER: i32 = SAVED_STACK + 24;

/// Room for the frame that the trampoline builds, which is as deep as any test needs.
const FRAME_AREA: i32 = 1 << 12;
/// What the trampoline gives back where the code left through an exit.
const LEFT: u64 = u64::MAX;

const MAX_ARGUMENTS: usize = 4;

pub(super) struct Harness {
    buffer: Vec<u64>,
    trampoline: CodeHandle,
    #[expect(dead_code, reason = "it keeps the code alive")]
    body: CodeHandle,
}

fn assembler() -> MacroAssembler {
    MacroAssembler::new(Options {
        features: Features::host(),
        shorten_jumps: true,
        seed: 0,
    })
}

impl Harness {
    pub(super) fn new(output: &Output) -> Self {
        let (trampoline, exit) = build_trampoline();
        let body = link(&output.code, Priority::Required, |which| {
            assert_eq!(which, symbol::EXIT);
            trampoline.start() + exit as usize
        })
        .unwrap();
        let mut buffer = vec![0u64; BUFFER_WORDS];
        buffer[TARGET as usize / 8] = (body.start() + output.entry as usize) as u64;
        Self {
            buffer,
            trampoline,
            body,
        }
    }

    fn enter(&mut self, arguments: &[u64]) -> u64 {
        assert!(arguments.len() <= MAX_ARGUMENTS);
        let mut given = [Value::UNDEFINED.to_bits(); MAX_ARGUMENTS];
        given[..arguments.len()].copy_from_slice(arguments);
        self.buffer[EXIT_NUMBER as usize / 8] = 0;
        let call = trampoline_of(self.trampoline.start());
        call(
            self.buffer.as_mut_ptr(),
            arguments.len() as u64,
            given[0],
            given[1],
            given[2],
            given[3],
        )
    }

    /// The value that the code returns. The first argument is `this`.
    pub(super) fn call(&mut self, arguments: &[u64]) -> u64 {
        let result = self.enter(arguments);
        assert_ne!(result, LEFT, "the code left through an exit");
        result
    }

    /// The number of the exit that the code took.
    pub(super) fn call_expecting_exit(&mut self, arguments: &[u64]) -> u32 {
        let result = self.enter(arguments);
        assert_eq!(result, LEFT, "the code returned {result:#x}");
        self.buffer[EXIT_NUMBER as usize / 8] as u32
    }
}

type Trampoline = extern "C" fn(*mut u64, u64, u64, u64, u64, u64) -> u64;

#[expect(unsafe_code, reason = "the tests run the code that they compile")]
fn trampoline_of(address: usize) -> Trampoline {
    // SAFETY: the trampoline below is emitted with this signature.
    unsafe { core::mem::transmute::<usize, Trampoline>(address) }
}

/// The trampoline and, as an offset into its code, the stub that an exit jumps to.
fn build_trampoline() -> (CodeHandle, u32) {
    let mut masm = assembler();
    let spare = abi::SAVED_GPRS[0];
    let scratch = abi::NON_ARGUMENT_GPRS[0];
    let (buffer, count) = (abi::ARGUMENT_GPRS[0], abi::ARGUMENT_GPRS[1]);
    let (done, stub) = (masm.new_label(), masm.new_label());

    masm.enter_frame();
    masm.push_pair(abi::VM, abi::NUMBER_TAG);
    masm.push_pair(abi::NOT_CELL_MASK, spare);
    masm.move64(abi::VM, buffer);
    masm.move64(abi::NUMBER_TAG, Value::NUMBER_TAG as i64);
    masm.move64(abi::NOT_CELL_MASK, Value::NOT_CELL_MASK as i64);
    masm.compute_address(spare, Address::new(abi::STACK_POINTER, 0));
    masm.store64(Address::new(abi::VM, SAVED_STACK), spare);
    masm.store64(Address::new(abi::VM, SAVED_FRAME), abi::FRAME_POINTER);

    // The callee's frame, with its header and the arguments it reads from memory.
    masm.compute_address(scratch, Address::new(abi::STACK_POINTER, -FRAME_AREA));
    let header = CallFrame::OFFSET_OF_CODE_BLOCK as i32;
    masm.store64(Address::new(scratch, header), 0);
    masm.store64(Address::new(scratch, CallFrame::OFFSET_OF_CALLEE as i32), 0);
    masm.store32(
        Address::new(scratch, CallFrame::OFFSET_OF_ARGUMENT_COUNT as i32),
        count,
    );
    masm.store32(
        Address::new(scratch, CallFrame::OFFSET_OF_CALL_SITE as i32),
        0,
    );
    for index in 0..MAX_ARGUMENTS {
        let offset = CallFrame::OFFSET_OF_THIS as i32 + index as i32 * 8;
        masm.store64(Address::new(scratch, offset), abi::ARGUMENT_GPRS[index + 2]);
    }
    // The same arguments, for a function whose parameters arrive in registers.
    for index in 0..MAX_ARGUMENTS {
        masm.move64(abi::ARGUMENT_GPRS[index], abi::ARGUMENT_GPRS[index + 2]);
    }
    masm.compute_address(spare, Address::new(scratch, CallFrame::LINKAGE_SIZE as i32));
    masm.load64(scratch, Address::new(abi::VM, TARGET));
    masm.move64(abi::STACK_POINTER, spare);
    masm.call_indirect(scratch);
    masm.jump(done);

    masm.bind(stub);
    masm.store32(Address::new(abi::VM, EXIT_NUMBER), EXIT_REGISTER);
    masm.load64(abi::FRAME_POINTER, Address::new(abi::VM, SAVED_FRAME));
    masm.move64(abi::RETURN_GPRS[0], LEFT as i64);

    masm.bind(done);
    masm.load64(spare, Address::new(abi::VM, SAVED_STACK));
    masm.move64(abi::STACK_POINTER, spare);
    masm.pop_pair(abi::NOT_CELL_MASK, spare);
    masm.pop_pair(abi::VM, abi::NUMBER_TAG);
    masm.leave_frame();
    masm.ret();

    let code = masm.finish().unwrap();
    let offset = code.offset_of(stub);
    let handle = link(&code, Priority::Required, |_| unreachable!("no symbol")).unwrap();
    (handle, offset)
}
