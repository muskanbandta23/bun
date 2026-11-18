//! What is in a microtask queue.

use bun_engine_macros::cell;

use super::tagged_field::{ADDRESS_MASK, TAG_SHIFT};
use crate::{Gc, GlobalObject, JsCell, Value, Visitor, Vm};

macro_rules! internal_microtasks {
    ($($(#[$doc:meta])* $name:ident,)*) => {
        /// The kinds of jobs. Unless it says otherwise, the payload is a `PromiseStatus` and the arguments are the
        /// `cell` of an `InternalReaction`, what the promise has settled with, the `context` and the `async_context`.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
        #[repr(u8)]
        pub enum InternalMicrotask {
            $($(#[$doc])* $name,)*
        }

        impl InternalMicrotask {
            const COUNT: usize = [$(stringify!($name),)*].len();
        }
    };
}

internal_microtasks! {
    /// A function, the async context, and up to two arguments.
    CallInAsyncContext,
    /// A function and up to three arguments.
    CallWithArguments,

    /// NewPromiseReactionJob. The context is the handler. See `HandlerContext` for the rest.
    PromiseReaction,
    /// The same, when there is no handler. The cell is the promise or the capability.
    PromiseResolveWithoutHandler,
    /// NewPromiseResolveThenableJob: the thenable, its `then`, a promise or a `PromiseReaction`, and the async context.
    PromiseResolveThenable,
    /// The same for a promise whose `then` is the original one: that promise, the promise to resolve, and the async
    /// context.
    PromiseResolveNativeThenable,
    /// An `InternalReaction` with its task in the payload. The promise to react to is in place of the value.
    PromiseReactToNativeThenable,
    /// The cell is the promise that `Promise.race` has returned, and so is the context.
    PromiseRaceElement,
    /// The cell is the `PromiseCombinator`, the context is the index.
    PromiseAllElement,
    PromiseAllSettledElement,
    PromiseAnyElement,
    /// The cell is the promise that `finally` has returned, the context is the callback.
    PromiseFinally,
    /// What the callback returned has settled. The context is what the original promise has settled with.
    PromiseFinallyFulfill,
    PromiseFinallyReject,

    /// The context is the `JsAsyncFunctionGenerator`.
    AsyncFunctionResume,
    /// The context is the `JsAsyncGenerator`.
    AsyncGeneratorResume,
    /// The operand of `yield` has settled.
    AsyncGeneratorYield,
    /// The argument of `return` has settled, and the body goes on with it.
    AsyncGeneratorResumeWithReturn,
    /// The same when there is no body to run.
    AsyncGeneratorAwaitReturn,
    /// The answer to `async_iterator_next`. The context is the `Coroutine` that asked. If the cell is an object, it is
    /// the iterator result to put the value in.
    AsyncIteratorAnswer,
    /// The cell is the promise or the `Coroutine` that waits, the context is the `JsAsyncFromSyncIterator`.
    AsyncFromSyncIteratorContinue,
    AsyncFromSyncIteratorContinueOrClose,
    AsyncFromSyncIteratorDone,

    // From here on, those of the module loader.

    /// Passes the value on without looking for a `then`. The cell is the promise.
    PromiseFulfillWithoutHandler,
    ImportModuleNamespace,
    AsyncModuleExecutionDone,
    ModuleRegistryFetchSettled,
    ModuleRegistryModuleSettled,
    ModuleGraphLoadingError,
    ModuleLoadStep,
    ModuleLoadTopSettled,
    ModuleLoadTopRejected,
    ModuleLoadSpecifierTransform,
    ModuleLoadCombinedLoadSettled,
    ModuleLoadCombinedStateSettled,
    ModuleLoadLinkEvaluateSettled,
    ModuleLoadReturnRecord,
    ModuleLoadStoreError,
    DynamicImportLoadSettled,
    DynamicImportEvaluateSettled,
    DynamicImportDeferDependencySettled,
}

impl InternalMicrotask {
    /// Panics if there is none.
    #[inline(always)]
    pub(super) fn from_bits(bits: u8) -> InternalMicrotask {
        assert!(usize::from(bits) < Self::COUNT);
        // SAFETY: the variants are numbered from zero.
        unsafe { core::mem::transmute(bits) }
    }

    pub(super) fn is_of_module_loader(self) -> bool {
        self >= InternalMicrotask::PromiseFulfillWithoutHandler
    }
}

/// Stands in for the global object of a task that a debugger follows.
#[cell]
pub(super) struct TrackedMicrotask {
    base: JsCell,
    global: Gc<GlobalObject>,
    id: u64,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct QueuedTask {
    /// The address of the global object, the job above it, and the payload in the top byte.
    header: u64,
    pub arguments: [Option<Value>; QueuedTask::MAX_ARGUMENTS],
}

impl QueuedTask {
    pub const MAX_ARGUMENTS: usize = 4;
    pub const OFFSET_OF_ARGUMENTS: usize = core::mem::offset_of!(QueuedTask, arguments);
    pub const JOB_SHIFT: u32 = TAG_SHIFT;
    pub const PAYLOAD_SHIFT: u32 = TAG_SHIFT + 8;
    const IS_TRACKED: u64 = 1;

    #[inline(always)]
    pub fn new(
        global: Gc<GlobalObject>,
        job: InternalMicrotask,
        payload: u8,
        arguments: [Option<Value>; QueuedTask::MAX_ARGUMENTS],
    ) -> QueuedTask {
        QueuedTask {
            header: global.address() as u64
                | (job as u64) << Self::JOB_SHIFT
                | u64::from(payload) << Self::PAYLOAD_SHIFT,
            arguments,
        }
    }

    #[inline(always)]
    pub fn job(&self) -> InternalMicrotask {
        InternalMicrotask::from_bits((self.header >> Self::JOB_SHIFT) as u8)
    }

    #[inline(always)]
    pub fn payload(&self) -> u8 {
        (self.header >> Self::PAYLOAD_SHIFT) as u8
    }

    #[inline(always)]
    pub(super) fn is_tracked(&self) -> bool {
        self.header & Self::IS_TRACKED != 0
    }

    #[inline(always)]
    fn cell(&self) -> Gc<JsCell> {
        let address = self.header & ADDRESS_MASK & !Self::IS_TRACKED;
        // SAFETY: what the task was made with. Wherever the task is, it is visited.
        unsafe { Gc::from_address(address as usize) }
    }

    #[inline(always)]
    pub fn global(&self) -> Gc<GlobalObject> {
        if self.is_tracked() {
            return self.cell().cast::<TrackedMicrotask>().global;
        }
        // SAFETY: as it was made.
        unsafe { self.cell().cast_unchecked() }
    }

    pub(super) fn tracked(self, vm: &Vm, id: u64) -> QueuedTask {
        let tracked = vm.allocate::<TrackedMicrotask>(TrackedMicrotaskInit {
            base: vm.shape_for::<TrackedMicrotask>(),
            global: self.global(),
            id,
        });
        QueuedTask {
            header: self.header & !ADDRESS_MASK | tracked.address() as u64 | Self::IS_TRACKED,
            ..self
        }
    }

    pub(super) fn tracking_id(&self) -> Option<u64> {
        Some(self.cell().downcast::<TrackedMicrotask>()?.id)
    }

    #[inline]
    pub(super) fn visit(&self, visitor: &mut Visitor) {
        visitor.append(self.cell());
        crate::Trace::trace(&self.arguments, visitor);
    }
}

const _: () = assert!(size_of::<QueuedTask>() == 40);
