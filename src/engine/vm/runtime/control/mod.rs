//! Promises, microtasks, generators and async functions.

mod async_context;
mod async_function;
mod async_generator;
mod async_iteration;
mod async_stack_trace;
mod combinator;
mod coroutine;
mod disposal;
mod generator;
mod jobs;
mod microtask;
mod microtask_queue;
mod promise;
mod reaction;
mod resolving;
mod synchronous_module_queue;
mod tagged_field;

use core::cell::{Cell, RefCell};
use std::rc::Weak;

pub use async_function::{JsAsyncFunctionGenerator, async_return, async_throw};
pub use async_generator::JsAsyncGenerator;
pub use async_iteration::{
    AsyncIteratorRecord, JsAsyncFromSyncIterator, open_async_iterator, request_async_step,
};
pub use combinator::PromiseCombinator;
pub use coroutine::{Coroutine, CoroutineInit, CoroutineState};
pub use disposal::{get_dispose_method, new_suppressed_error};
pub use generator::JsGenerator;
pub use microtask::{InternalMicrotask, QueuedTask};
pub(crate) use microtask_queue::RealmMicrotaskQueue;
pub use microtask_queue::{MicrotaskEvent, MicrotaskHooks, MicrotaskQueue, MicrotaskQueueRef};
pub use promise::{InlineReactionKind, JsPromise, PromiseStatus, is_definitely_not_thenable};
pub use reaction::{FullPromiseReaction, InternalReaction, PromiseReaction};
pub use resolving::{JsFunctionWithFields, PromiseCapability};
pub use synchronous_module_queue::SynchronousModuleQueueScope;

use crate::heap::Scope;
use crate::vm::Root;
use crate::{Sentinel, Visitor, Vm};

pub(crate) struct State {
    has_synchronous_module_queue: Cell<bool>,
    async_iteration_sentinel: Root<Sentinel>,
    /// For those that have not come to their `await` yet.
    early_answers: MicrotaskQueue,
    resolver_executables: resolving::ResolverExecutables,
    queues: RefCell<Vec<Weak<MicrotaskQueue>>>,
    synchronous_module_queues: RefCell<Vec<MicrotaskQueue>>,
    last_microtask_id: Cell<u64>,
}

impl Default for State {
    fn default() -> State {
        State {
            has_synchronous_module_queue: Cell::new(false),
            async_iteration_sentinel: Root::new(),
            early_answers: MicrotaskQueue::new(),
            resolver_executables: Default::default(),
            queues: RefCell::default(),
            synchronous_module_queues: RefCell::default(),
            last_microtask_id: Cell::new(0),
        }
    }
}

impl crate::services::Service for State {
    fn initialize(&self, vm: &Vm) {
        vm.default_microtask_queue().register(vm);
        (self.async_iteration_sentinel).set(Some(Sentinel::create(vm)));
    }

    /// Those that they were for have been terminated.
    #[inline]
    fn will_enter(&self, _: &Vm) {
        self.early_answers.clear();
    }

    fn visit(&self, visitor: &mut Visitor) {
        self.async_iteration_sentinel.visit(visitor);
        self.resolver_executables.visit(visitor);
        self.early_answers.visit(visitor);
        for queue in self.synchronous_module_queues.borrow().iter() {
            queue.visit(visitor);
        }
        self.queues
            .borrow_mut()
            .retain(|queue| queue.upgrade().inspect(|it| it.visit(visitor)).is_some());
    }

    fn did_collect(&self, vm: &Vm, scope: Scope) {
        if scope == Scope::Full {
            vm.default_microtask_queue().shrink();
        }
    }
}
