//! What the host needs to walk and unwind the stack. Only the frames of stubs are ever at the top.

use core::cell::Cell;
use core::mem::offset_of;
use core::num::NonZeroUsize;
use core::ptr;

use bun_engine_macros::native_abi;
use bun_engine_wasm_compiler::abi::frame;
use bun_engine_wasm_format::FuncIndex;

use crate::code::InstalledCode;
use crate::engine::Engine;
use crate::instance::Instance;
use crate::outcome::Failure;
use crate::value::HostCell;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Frame {
    pub fp: usize,
    /// From the frame below it. Zero if there is none.
    pub return_address: usize,
}

/// Where an exception goes. The code gets the frame pointer and the registers that are pinned in JavaScript.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Catch {
    pub fp: usize,
    pub address: usize,
}

/// From `find_handler` to the stub that resumes.
#[derive(Default)]
pub(crate) struct Handler {
    address: Cell<usize>,
    stack_pointer: Cell<usize>,
}

impl Handler {
    pub(crate) const ADDRESS: i32 = offset_of!(Self, address) as i32;
    pub(crate) const STACK_POINTER: i32 = offset_of!(Self, stack_pointer) as i32;
}

impl Frame {
    /// # Safety
    /// The frame is on a stack for `'a`.
    unsafe fn code<'a>(self) -> Option<&'a InstalledCode> {
        // SAFETY: the caller's.
        unsafe { InstalledCode::running_at(self.return_address.checked_sub(1)?) }
    }
}

impl Engine {
    /// # Safety
    /// The frame at `fp` is that of a function or has `STUB_CALLEE`, and is on a stack for `'a`.
    pub unsafe fn instance_of_frame<'a>(&self, fp: usize) -> &'a Instance {
        let place = ptr::with_exposed_provenance::<usize>(fp + frame::INSTANCE as usize);
        // SAFETY: the prologue wrote it, and the collector finds it there.
        let instance = unsafe {
            let cell = NonZeroUsize::new(place.read());
            cell.and_then(|cell| Instance::of_cell(self, HostCell(cell)))
        };
        instance.expect("an instance")
    }

    /// Where to jump with the frame pointer of the frame, before this is called again, if it handles exceptions.
    ///
    /// # Safety
    /// The frame is on the stack of this thread.
    pub unsafe fn find_handler(&self, frame: Frame) -> Option<usize> {
        // SAFETY: the caller's.
        let code = unsafe { frame.code() }?;
        let address = code.handler(frame.return_address)?;
        // SAFETY: the code tells.
        let handler = unsafe { self.instance_of_frame(frame.fp) }.handler();
        handler.address.set(address);
        handler.stack_pointer.set(frame.fp - code.frame_size());
        Some(self.stubs().resume)
    }

    /// The functions that are active in the frame, the innermost first. None for a stub.
    ///
    /// # Safety
    /// The frame is on a stack for `'a`.
    pub unsafe fn functions_in<'a>(&self, frame: Frame) -> impl Iterator<Item = FuncIndex> + 'a {
        // SAFETY: the caller's.
        let code = unsafe { frame.code() };
        code.into_iter()
            .flat_map(move |code| code.functions_at(frame.return_address))
    }
}

/// # Safety
/// `fp` is that of the innermost frame, in which a stub runs.
#[native_abi]
pub(crate) unsafe fn raise(instance: &Instance, failure: Failure, fp: usize) -> Catch {
    instance.store().set_top_frame(fp);
    let (host, vm) = instance.store().host();
    if let Some(trap) = failure.as_trap() {
        host.throw_trap(instance, trap);
    }
    // SAFETY: the caller's.
    unsafe { host.unwind(vm, fp) }
}
