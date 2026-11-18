//! Requests that interrupt running code. Any thread can make them.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use bun_engine_macros::FieldOffsets;

use crate::{Cx, JsResult};

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct Trap: u32 {
        const SHELL_TIMEOUT_CHECK = 1 << 0;
        const TERMINATION = 1 << 1;
        const DEBUGGER_BREAK = 1 << 2;
        const STOP_THE_WORLD = 1 << 3;
    }
}

#[derive(FieldOffsets)]
#[repr(C)]
pub struct Traps {
    pending: AtomicU32,
    is_termination_requested: AtomicBool,
    /// What generated code compares the stack pointer with. The highest address while a request is pending.
    stack_limit: AtomicUsize,
    real_stack_limit: AtomicUsize,
}

impl Traps {
    pub(crate) const OFFSET_OF_STACK_LIMIT: usize = core::mem::offset_of!(Traps, stack_limit);
    pub(crate) const OFFSET_OF_REAL_STACK_LIMIT: usize =
        core::mem::offset_of!(Traps, real_stack_limit);
    pub(crate) const OFFSET_OF_PENDING: usize = core::mem::offset_of!(Traps, pending);

    pub(crate) const fn new() -> Traps {
        Traps {
            stack_limit: AtomicUsize::new(0),
            real_stack_limit: AtomicUsize::new(0),
            pending: AtomicU32::new(0),
            is_termination_requested: AtomicBool::new(false),
        }
    }

    pub fn fire(&self, trap: Trap) {
        self.pending.fetch_or(trap.bits(), Ordering::SeqCst);
        self.stack_limit.store(usize::MAX, Ordering::SeqCst);
        if trap.contains(Trap::TERMINATION) {
            crate::interrupt_wait(self);
        }
    }

    pub fn request_termination(&self) {
        self.is_termination_requested.store(true, Ordering::SeqCst);
        self.fire(Trap::TERMINATION);
    }

    #[inline(always)]
    pub fn stack_limit(&self) -> usize {
        self.stack_limit.load(Ordering::Relaxed)
    }

    #[inline(always)]
    pub fn need_handling(&self) -> bool {
        self.pending.load(Ordering::Relaxed) != 0
    }

    pub fn is_pending(&self, trap: Trap) -> bool {
        self.pending.load(Ordering::Relaxed) & trap.bits() != 0
    }

    #[inline(always)]
    pub fn termination_flag(&self) -> &AtomicBool {
        &self.is_termination_requested
    }

    /// Whether it was pending.
    pub fn clear(&self, trap: Trap) -> bool {
        let old = self.pending.fetch_and(!trap.bits(), Ordering::SeqCst);
        self.update_stack_limit();
        old & trap.bits() != 0
    }

    #[inline(always)]
    pub(crate) fn real_stack_limit(&self) -> usize {
        self.real_stack_limit.load(Ordering::Relaxed)
    }

    pub(crate) fn set_real_stack_limit(&self, limit: usize) {
        self.real_stack_limit.store(limit, Ordering::Relaxed);
        self.update_stack_limit();
    }

    fn update_stack_limit(&self) {
        self.stack_limit
            .store(self.real_stack_limit(), Ordering::SeqCst);
        if self.pending.load(Ordering::SeqCst) != 0 {
            self.stack_limit.store(usize::MAX, Ordering::SeqCst);
        }
    }
}

impl Cx {
    #[cold]
    #[inline(never)]
    pub fn handle_traps(&mut self) -> JsResult<()> {
        if self.traps().clear(Trap::DEBUGGER_BREAK)
            && let Some(handler) = self.embedder().debugger_break.get()
        {
            handler(self);
        }
        if self.traps().clear(Trap::SHELL_TIMEOUT_CHECK)
            && let Some(handler) = self.embedder().shell_timeout_check.get()
        {
            handler(self);
        }
        if self.traps().clear(Trap::TERMINATION) {
            if !self.is_termination_deferred() {
                return Err(self.throw_termination());
            }
            self.did_defer_termination();
        }
        self.check_exception()
    }
}
