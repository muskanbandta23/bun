//! What a debugger hears from running code, and what it can ask of it.

mod frame;
mod native_calls;

use core::any::Any;
use core::cell::Cell;
use std::rc::Rc;

use bun_engine_bytecode::DebugHook;

pub use frame::DebuggerFrame;
pub(crate) use native_calls::will_call_native_function;

use crate::{
    CallFrame, CodeBlock, CompileError, Coroutine, Cx, Exception, Gc, GcFree, GlobalObject,
    InternalMicrotask, MicrotaskEvent, SourceProvider, Vm,
};

/// Attached to a realm, which is that of `cx`. No exception is pending when a method is called, nor when it returns.
pub trait Debugger: GcFree + Any {
    fn did_parse(&self, cx: &Cx, provider: &SourceProvider, error: Option<&CompileError>);

    /// Code with debug hooks.
    fn did_link(&self, cx: &Cx, code_block: &CodeBlock);

    /// The instruction `debug`.
    fn did_reach_hook(&self, cx: &mut Cx, frame: &CallFrame, hook: DebugHook);

    /// The instruction `debug_await`.
    fn did_reach_suspension(
        &self,
        cx: &Cx,
        frame: &CallFrame,
        hook: DebugHook,
        coroutine: Gc<Coroutine>,
    );

    /// A tail call takes the place of the frame.
    fn will_replace_frame(&self, cx: &Cx, frame: &CallFrame);

    /// Once for each exception. The innermost frame is the one that throws it.
    fn will_throw(&self, cx: &mut Cx, exception: Gc<Exception>);

    fn did_unwind_frame(&self, cx: &mut Cx, frame: &CallFrame);

    /// See `Vm::set_watches_native_calls`.
    fn will_call_native_function(&self, cx: &mut Cx, frame: &CallFrame);

    /// With a number that tells the task from all others.
    fn microtask_event(&self, vm: &Vm, event: MicrotaskEvent, job: InternalMicrotask, id: u64);
}

#[derive(Default)]
pub(crate) struct State {
    watches_native_calls: Cell<bool>,
    allows_redeclaration: Cell<bool>,
}

impl crate::services::Service for State {}

impl GlobalObject {
    #[inline]
    pub fn debugger(&self) -> Option<Rc<dyn Debugger>> {
        self.debugger_slot().borrow().clone()
    }

    pub fn set_debugger(&self, vm: &Vm, debugger: Option<Rc<dyn Debugger>>) {
        let gained = u32::from(debugger.is_some());
        let lost = u32::from(self.debugger_slot().replace(debugger).is_some());
        vm.debugger_count().update(|count| count + gained - lost);
    }
}

impl Vm {
    /// Whether a script may declare a lexical binding that the global scope has. Returns the old setting.
    pub fn replace_allows_redeclaration(&self, value: bool) -> bool {
        self.services.debugger.allows_redeclaration.replace(value)
    }

    pub(crate) fn allows_redeclaration(&self) -> bool {
        self.services.debugger.allows_redeclaration.get()
    }
}

impl CallFrame {
    /// `None` for WebAssembly code.
    fn realm(&self) -> Option<Gc<GlobalObject>> {
        match self.code_block() {
            Some(code_block) => Some(code_block.global()),
            None if self.has_native_callee() => None,
            None => self.callee().realm(),
        }
    }
}

impl Cx {
    fn notify_debugger(&mut self, notify: impl FnOnce(&mut Cx, &dyn Debugger)) {
        let Some(debugger) = self.global().debugger() else {
            return;
        };
        let _defer = self.defer_termination();
        let (pending, last) = (self.clear_exception(), self.last_exception());
        notify(self, &*debugger);
        debug_assert!(!self.has_exception());
        self.restore_exceptions(pending, last);
    }

    /// The instruction `debug`, when it does something.
    pub fn debug(&mut self, frame: &CallFrame, hook: DebugHook) {
        self.notify_debugger(|cx, debugger| debugger.did_reach_hook(cx, frame, hook));
    }

    /// The instruction `debug_await`, when it does something.
    pub fn debug_await(&self, frame: &CallFrame, hook: DebugHook, coroutine: Gc<Coroutine>) {
        if let Some(debugger) = self.global().debugger() {
            debugger.did_reach_suspension(self, frame, hook, coroutine);
        }
    }

    /// The instruction `jump_if_frame_is_kept`.
    pub fn keeps_frame_at_tail_call(&self, frame: &CallFrame) -> bool {
        const MAX_KEPT_IN_A_ROW: usize = 128;
        let Some(debugger) = self.global().debugger() else {
            return false;
        };
        let callers = self.machine_frames().skip(1);
        let kept = callers.take_while(|it| it.is_kept_for_debugger());
        let keeps = kept.take(MAX_KEPT_IN_A_ROW).count() < MAX_KEPT_IN_A_ROW;
        if !keeps {
            debugger.will_replace_frame(self, frame);
        }
        keeps
    }

    /// For code that throws where a script could run as well. What leaves a frame is reported on the way out.
    #[inline]
    pub fn notify_debugger_of_exception(&mut self) {
        if self.has_debugger() {
            self.notify_debugger_of_exception_slow();
        }
    }

    #[cold]
    #[inline(never)]
    fn notify_debugger_of_exception_slow(&mut self) {
        if let Some(exception) = self.exception()
            && !self.is_termination_exception(exception)
            && !exception.did_notify_debugger().replace(true)
        {
            self.notify_debugger(|cx, debugger| debugger.will_throw(cx, exception));
        }
    }
}

/// # Safety
/// The caller has the right to run scripts.
#[cold]
#[inline(never)]
pub(crate) unsafe fn will_unwind(vm: &Vm) {
    if let Some(global) = vm.machine_frames().find_map(CallFrame::realm) {
        // SAFETY: the caller's.
        unsafe { Cx::from_global(global) }.notify_debugger_of_exception_slow();
    }
}

/// # Safety
/// As above.
#[cold]
#[inline(never)]
pub(crate) unsafe fn did_unwind_frame(frame: &CallFrame) {
    if let Some(global) = frame.realm() {
        // SAFETY: the caller's.
        let cx = unsafe { Cx::from_global(global) };
        cx.notify_debugger(|cx, debugger| debugger.did_unwind_frame(cx, frame));
    }
}
