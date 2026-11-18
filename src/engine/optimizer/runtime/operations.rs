//! What compiled code calls where an instruction cannot do the work itself. Each is a thin wrapper that the code
//! reaches by address, so every one follows the C convention.
//!
//! A function that gives a value returns its bits, and zero means that an exception is pending. One that gives a
//! boolean or nothing returns 0 or 1, and 2 means that an exception is pending.

use bun_engine_macros::native_abi;
use bun_engine_vm::heap::{Allocator, barrier};
use bun_engine_vm::{CallFrame, CodeBlock, Cx, Gc, JsCell, Vm};

macro_rules! addresses {
    ($($name:ident,)*) => {
        /// What generated code calls, by address.
        pub(crate) mod address {
            $(pub(crate) fn $name() -> usize {
                super::$name as *const () as usize
            })*
        }
    };
}

addresses! {
    to_int32,
    allocate_slow,
    write_barrier_slow,
    handle_traps,
    throw_stack_overflow,
}

/// Three answers where the language has two.
pub(crate) mod answer {
    pub(crate) const FALSE: u32 = 0;
    pub(crate) const THREW: u32 = 2;
}

/// The realm of the code that the frame is running. Compiled code has made the frame known to the `Vm` already.
///
/// # Safety
/// The header of a frame whose code block is that of running code.
unsafe fn realm_of<'a>(frame: &CallFrame) -> &'a mut Cx {
    // SAFETY: the caller's. The frame keeps its code block alive, and generated code runs inside a call that
    // holds the `Cx`.
    unsafe {
        let code_block = Gc::<CodeBlock>::from_address(frame.code_block_bits());
        Cx::from_global(code_block.global())
    }
}

/// Does what a trap that the run time has raised asks for. Non-zero if an exception is pending afterwards.
#[native_abi]
pub(crate) unsafe fn handle_traps(frame: &CallFrame) -> u32 {
    // SAFETY: the caller's.
    let cx = unsafe { realm_of(frame) };
    u32::from(cx.handle_traps().is_err())
}

/// ToInt32 of a double that rounding toward zero cannot give an i64 for.
#[native_abi]
pub(crate) fn to_int32(value: f64) -> i32 {
    bun_engine_number::to_int32(value)
}

/// A cell of the allocator's size, once bumping its own interval has used it up.
#[native_abi]
pub(crate) fn allocate_slow(vm: &Vm, allocator: &Allocator) -> usize {
    let cell = allocator.allocate_slow(&vm.heap);
    if cell == 0 {
        bun_alloc::out_of_memory();
    }
    cell
}

/// The stack has no room for the frame that a call has just built. The frame has a header and nothing else.
#[native_abi]
pub(crate) unsafe fn throw_stack_overflow(vm: &Vm, frame: &CallFrame) {
    vm.set_top_call_frame(frame);
    frame.set_call_site(0);
    // SAFETY: the caller's. The code block is still the one of the callee.
    unsafe {
        let cx = realm_of(frame);
        frame.set_code_block(None);
        let _ = cx.throw_stack_overflow();
    }
}

/// Follows a store into a cell whose state is at most the barrier threshold.
#[native_abi]
pub(crate) fn write_barrier_slow(cell: &JsCell) {
    barrier::slow_path(cell);
}
