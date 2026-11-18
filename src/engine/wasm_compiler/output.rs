//! What goes into a compiler and what comes out.

use bun_engine_asm::{Code, Features};
use bun_engine_wasm_format::{CodeIndex, FuncIndex, Module, ValType};

use crate::abi::{HostLayout, InstanceLayout, MemoryMode, Stubs};

pub struct Environment<'a> {
    pub module: &'a Module,
    pub layout: &'a InstanceLayout,
    pub host: &'a HostLayout,
    pub stubs: &'a Stubs,
    pub features: Features,
    pub mode: MemoryMode,
    /// The address of the `FunctionSlot` of the first function in the code section.
    pub slots: usize,
    pub seed: u64,
}

/// A place in the code at which the stack can be walked.
#[derive(Clone, Copy, Debug)]
pub struct CallSite {
    pub return_offset: u32,
    /// Where to resume with an exception, or `u32::MAX` to leave the function.
    pub handler_offset: u32,
    /// A position in `inline_frames`, or `u32::MAX` for the function itself.
    pub inline_frame: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct InlineFrame {
    pub func: FuncIndex,
    /// A position in `inline_frames`, or `u32::MAX` for the function itself.
    pub caller: u32,
}

/// A patchable call or jump that goes through the slot of the callee until it is pointed at the code.
#[derive(Clone, Copy, Debug)]
pub struct DirectCall {
    pub end_offset: u32,
    pub callee: CodeIndex,
}

/// Where baseline code has its values at the start of a loop.
pub struct LoopEntry {
    pub loop_index: u32,
    /// The locals, then the operand stack from the bottom, then the exceptions that enclosing `catch` blocks hold.
    /// Offsets are from the frame pointer.
    pub values: Vec<(ValType, i32)>,
}

pub struct CompiledFunction {
    pub code: Code,
    pub entry_offset: u32,
    /// From the stack pointer up to the frame pointer.
    pub frame_size: u32,
    /// Ascending. A return address that is not listed has no handler and is in the function itself.
    pub call_sites: Vec<CallSite>,
    pub inline_frames: Vec<InlineFrame>,
    /// Ascending. From each offset up to the next, the code belongs to the frame, for faults.
    pub inline_ranges: Vec<(u32, u32)>,
    pub direct_calls: Vec<DirectCall>,
}
