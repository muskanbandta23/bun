//! Installing code, and what it calls.

use std::hash::{BuildHasher, RandomState};
use std::rc::Rc;
use std::sync::Arc;

use bun_engine_bytecode::Register;
use bun_engine_exec_memory::{CodeHandle, Priority};
use bun_engine_macros::native_abi;
use bun_engine_vm::heap::{Survivors, barrier};
use bun_engine_vm::{
    Arithmetic, ArithmeticProfile, CallFrame, CodeBlock, CodeOrigin, CompiledCode,
    FunctionExecutable, InlineFrame, JsCell, Tier, Value, ValueVec, Visitor, Vm, options,
};

use crate::{compiler, thunks};

struct Start {
    offset: u32,
    code: u32,
    /// The offset of a register, or zero.
    carried: i32,
}

/// Serves every code block that has the same unlinked code.
struct BaselineCode {
    handle: CodeHandle,
    entry: u32,
    starts: Box<[Start]>,
    return_points: Box<[(u32, u32)]>,
    assumed: Box<[(u32, Arithmetic)]>,
}

struct Installed(Arc<BaselineCode>);

impl CompiledCode for Installed {
    fn origin_of_call_site(&self, _: u32) -> CodeOrigin {
        unreachable!()
    }

    fn inline_frame(&self, _: &Vm, _: u32) -> InlineFrame {
        unreachable!()
    }

    unsafe fn inline_callee(&self, _: &CallFrame, _: u32) -> Value {
        unreachable!()
    }

    unsafe fn inline_arguments(&self, _: &CallFrame, _: u32, _: &mut ValueVec) {
        unreachable!()
    }

    fn catch_address(&self, _: u32, _: CodeOrigin, _: u32) -> usize {
        unreachable!()
    }

    fn visit(&self, _: &CodeBlock, _: &mut Visitor) {}

    fn reconcile_weak_references(&self, _: &CodeBlock, _: &Survivors) {}

    fn invalidate(&self) {}

    fn size(&self) -> usize {
        let code = &self.0;
        code.handle.len() + size_of_val(&*code.starts) + size_of_val(&*code.return_points)
    }
}

fn shared_code(code_block: &CodeBlock) -> Option<Arc<BaselineCode>> {
    let shared = code_block.code().machine_code.0.get()?;
    Arc::clone(shared).downcast().ok()
}

fn describe(code_block: &CodeBlock) -> String {
    match code_block.executable().downcast::<FunctionExecutable>() {
        Some(function) => format!("baseline {}", function.name_text()),
        None => format!("baseline {:?}", code_block.body().kind()),
    }
}

fn compile(code_block: &CodeBlock) -> Option<Arc<BaselineCode>> {
    let seed = RandomState::new().hash_one(());
    let compiled = compiler::compile(code_block, thunks::get()?, seed).ok()?;
    let handle = bun_engine_exec_memory::link(&compiled.code, Priority::Optional, |_| 0).ok()?;
    handle.describe(|| describe(code_block));
    let place = |label| compiled.code.offset_of(label);
    let mut return_points: Vec<_> = (compiled.return_points.iter())
        .map(|&(offset, label)| (offset, place(label)))
        .collect();
    return_points.sort_unstable();
    let mut assumed = compiled.assumed;
    assumed.dedup();
    let code = Arc::new(BaselineCode {
        handle,
        entry: place(compiled.entry),
        starts: (compiled.starts.iter())
            .map(|start| Start {
                offset: start.offset,
                code: place(start.label),
                carried: start.carried.map_or(0, Register::offset),
            })
            .collect(),
        return_points: return_points.into(),
        assumed: assumed.into(),
    });
    let _ = (code_block.code().machine_code.0).set(Arc::<BaselineCode>::clone(&code));
    Some(code)
}

/// For a code block that is being interpreted. False if it has to stay that way.
pub fn install(code_block: &CodeBlock) -> bool {
    let code = (options().use_jit && options().use_baseline_jit)
        .then(|| shared_code(code_block).or_else(|| compile(code_block)))
        .flatten();
    let Some(code) = code else {
        return false;
    };
    for &(word, seen) in &code.assumed {
        // SAFETY: from an instruction of the same code that has such an entry.
        unsafe { code_block.metadata::<ArithmeticProfile>(word) }.record(seen);
    }
    let (executable, specialization) = (code_block.executable(), code_block.specialization());
    if executable.code_block(specialization) == Some(code_block.into()) {
        let entry = code.handle.address(code.entry);
        // SAFETY: compiled from it.
        unsafe { executable.install(specialization, entry, Some(code_block.into())) };
    }
    code_block.set_compiled(Tier::Baseline, Rc::new(Installed(code)));
    (code_block.execution_counter()).set(-options().threshold_for_quick_optimizer);
    true
}

fn installed_code(code_block: &CodeBlock) -> Option<Arc<BaselineCode>> {
    shared_code(code_block).filter(|_| code_block.tier() == Tier::Baseline)
}

/// For the executable. This and the others below fall back on the interpreter if there is no baseline code.
pub fn entry(code_block: &CodeBlock) -> usize {
    match installed_code(code_block) {
        Some(code) => code.handle.address(code.entry),
        None => bun_engine_interpreter::entry(),
    }
}

/// To jump to with the frame pointer and the `Vm` in their registers. Runs the instruction at the call site.
pub fn resume_entry(code_block: &CodeBlock) -> usize {
    match thunks::get().filter(|_| code_block.tier() == Tier::Baseline) {
        Some(thunks) => thunks.resume,
        None => bun_engine_interpreter::resume_entry(),
    }
}

pub fn catch_address(code_block: &CodeBlock, _: u32) -> usize {
    match thunks::get().filter(|_| code_block.tier() == Tier::Baseline) {
        Some(thunks) => thunks.catch,
        None => bun_engine_interpreter::catch_entry(),
    }
}

/// Where a function that the instruction has called returns to. In baseline code, a getter or a setter too.
pub fn return_point(code_block: &CodeBlock, offset: u32) -> Option<usize> {
    let Some(code) = installed_code(code_block) else {
        return bun_engine_interpreter::return_point(code_block.body().instructions(), offset);
    };
    let index = (code.return_points).binary_search_by_key(&offset, |&(offset, _)| offset);
    Some(code.handle.address(code.return_points[index.ok()?].1))
}

#[repr(C)]
pub(crate) struct Resumption {
    address: usize,
    carried: u64,
}

#[native_abi]
pub(crate) unsafe fn find_start(frame: &CallFrame, offset: u32) -> Resumption {
    let code = frame.code_block().and_then(|it| shared_code(&it));
    let code = code.expect("baseline code");
    let index = (code.starts).binary_search_by_key(&offset, |start| start.offset);
    let start = &code.starts[index.expect("the start of an instruction")];
    Resumption {
        address: code.handle.address(start.code),
        // SAFETY: a register of the code, or the first word of the frame.
        carried: unsafe { frame.slot(start.carried as isize) }.get(),
    }
}

#[native_abi]
pub(crate) fn remember(cell: &JsCell) {
    barrier::slow_path(cell);
}

fn tier_up(vm: &Vm, frame: &CallFrame, loop_offset: Option<u32>) -> usize {
    let code_block = frame.code_block().expect("a frame of baseline code");
    vm.set_top_call_frame(frame);
    frame.set_call_site(code_block.call_site_of(loop_offset.unwrap_or(0)));
    (vm.hooks().tier_up)(vm, &code_block, frame, loop_offset)
}

#[native_abi]
pub(crate) unsafe fn finish_prologue(vm: &Vm, frame: &CallFrame) -> usize {
    let code_block = frame.code_block().expect("a frame of baseline code");
    code_block.write_barrier_for_cell();
    if code_block.execution_counter().get() < 0 {
        return 0;
    }
    tier_up(vm, frame, None)
}

#[native_abi]
pub(crate) unsafe fn tier_up_in_loop(vm: &Vm, frame: &CallFrame, offset: u32) -> usize {
    tier_up(vm, frame, Some(offset))
}
