//! `for await` and `yield*` in an async generator. If nothing can tell the difference, the iterator answers the code
//! that asks for the next result directly, in the tick in which the promise for it would have.

use bun_engine_macros::cell;
use bun_engine_text::{names, symbols};

use crate::{
    Coroutine, CoroutineState, Cx, Field, Gc, InternalMicrotask, InternalReaction,
    JsAsyncFunctionGenerator, JsAsyncGenerator, JsObject, JsObjectInit, JsPromise, JsResult,
    PromiseStatus, PropertyOffset, QueuedTask, RealmWatchpoint, Sentinel, Value, Vm, errors,
};

/// In the payload of an `AsyncIteratorAnswer`.
const IS_DONE: u8 = 1 << 2;

impl Vm {
    /// In place of `next`, and of the promise that it would return.
    #[inline(always)]
    pub fn async_iteration_sentinel(&self) -> Gc<Sentinel> {
        // SAFETY: set when the `Vm` is made.
        unsafe { (self.services.control.async_iteration_sentinel.get()).unwrap_unchecked() }
    }
}

pub struct AsyncIteratorRecord {
    pub iterator: Gc<JsObject>,
    pub next: Value,
}

/// `method` is `iterable[Symbol.asyncIterator]`. `None` if it has to be called.
pub fn open_async_iterator(
    cx: &mut Cx,
    iterable: Value,
    method: Value,
) -> JsResult<Option<AsyncIteratorRecord>> {
    let sentinel = cx.async_iteration_sentinel().into();
    if method.is_undefined_or_null() {
        let iterator = JsAsyncFromSyncIterator::for_iterable(cx, iterable)?.upcast();
        let next = if cx.holds(RealmWatchpoint::PromiseSpecies) {
            sentinel
        } else {
            cx.async_from_sync_iterator_prototype_next().into()
        };
        return Ok(Some(AsyncIteratorRecord { iterator, next }));
    }
    if let Some(generator) = iterable.downcast::<JsAsyncGenerator>()
        && method == cx.async_iterator_prototype_async_iterator().into()
        && cx.holds(RealmWatchpoint::PromiseSpecies)
        && generator.get_pure(cx, names::next) == Some(cx.async_generator_prototype_next().into())
    {
        return Ok(Some(AsyncIteratorRecord {
            iterator: generator.upcast(),
            next: sentinel,
        }));
    }
    Ok(None)
}

/// What `asker` has to await. Fails only if execution is terminated.
pub fn request_async_step(
    cx: &mut Cx,
    iterator: Value,
    asker: Gc<Coroutine>,
    value: Option<Value>,
) -> JsResult<Value> {
    // Awaiting a promise reads its `constructor`.
    let (target, awaited) = if cx.holds(RealmWatchpoint::PromiseSpecies) {
        (asker.upcast(), cx.async_iteration_sentinel().into())
    } else {
        let promise = JsPromise::new(cx);
        (promise.upcast(), promise.into())
    };
    match iterator.downcast::<JsAsyncGenerator>() {
        Some(generator) => generator.request(cx, target, value.unwrap_or_default())?,
        None => (iterator.cast::<JsAsyncFromSyncIterator>()).request(cx, target, value)?,
    }
    Ok(awaited)
}

/// In the realm of the iterator. The one who asks reads the result at once and does not pass it on, so one object does
/// for all steps. Returns that.
pub(super) fn answer(
    cx: &mut Cx,
    asker: Gc<JsObject>,
    value: Value,
    done: bool,
    reusable: Option<Gc<JsObject>>,
    async_context: Value,
) -> JsResult<Option<Gc<JsObject>>> {
    let task = InternalMicrotask::AsyncIteratorAnswer;
    if !cx.holds(RealmWatchpoint::PromiseThen) {
        let result = cx.new_iterator_result(value, done);
        InternalReaction::new(task, None, asker.into())
            .in_async_context(async_context)
            .resolve(cx, result.into())?;
        return Ok(None);
    }
    let result = reusable.unwrap_or_else(|| cx.new_iterator_result(Value::UNDEFINED, false));
    let payload = PromiseStatus::Fulfilled as u8 | if done { IS_DONE } else { 0 };
    let arguments = [result.into(), value, asker.into(), async_context];
    cx.global().queue_microtask(cx, task, payload, arguments);
    Ok(Some(result))
}

pub(super) fn answer_with_error(cx: &Cx, asker: Gc<JsObject>, error: Value) {
    InternalReaction::new(InternalMicrotask::AsyncIteratorAnswer, None, asker.into())
        .in_current_async_context(cx)
        .queue(cx, &cx.global(), PromiseStatus::Rejected, error);
}

#[inline(never)]
pub(super) fn run_answer(cx: &mut Cx, payload: u8, arguments: [Value; 4]) -> JsResult<()> {
    let [result, mut value, asker, async_context] = arguments;
    let asker = asker.cast::<Coroutine>();
    if asker.state() != CoroutineState::SuspendedAwait {
        keep_early_answer(cx, payload, arguments);
        return Ok(());
    }
    let _scope = super::async_context::AsyncContextScope::enter(cx, async_context);
    if let Some(result) = result.as_object() {
        let done = Value::from(payload & IS_DONE != 0);
        result.put_direct_at(cx, PropertyOffset::new(0), value);
        result.put_direct_at(cx, PropertyOffset::new(1), done);
        value = result.into();
    }
    let status = PromiseStatus::from_payload(payload);
    match asker.downcast::<JsAsyncFunctionGenerator>() {
        Some(function) => function.continue_with(cx, status, value),
        None => {
            let task = InternalMicrotask::AsyncIteratorAnswer;
            (asker.cast::<JsAsyncGenerator>()).continue_with(cx, task, status, value)
        }
    }
}

/// The iterator has drained the microtasks before it returned to the one who asked.
#[cold]
fn keep_early_answer(cx: &Cx, payload: u8, arguments: [Value; 4]) {
    let job = InternalMicrotask::AsyncIteratorAnswer;
    let task = QueuedTask::new(cx.global(), job, payload, arguments.map(Some));
    cx.services.control.early_answers.enqueue(task);
}

#[inline(always)]
pub(super) fn requeue_early_answer(vm: &Vm, asker: &Coroutine) {
    if !vm.services.control.early_answers.is_empty() {
        requeue_early_answer_slow(vm, asker);
    }
}

#[cold]
#[inline(never)]
fn requeue_early_answer_slow(vm: &Vm, asker: &Coroutine) {
    let answers = &vm.services.control.early_answers;
    for task in (0..answers.len()).map_while(|_| answers.dequeue()) {
        if task.arguments[2] == Some(asker.into()) {
            super::microtask_queue::queue_tracked(vm, &task);
        } else {
            answers.enqueue(task);
        }
    }
}

/// Scripts never see it.
#[cell(name = "AsyncFromSyncIterator", cell_type = AsyncFromSyncIterator,
    export = "engine_class_AsyncFromSyncIterator")]
pub struct JsAsyncFromSyncIterator {
    base: JsObject,
    sync_iterator: Gc<JsObject>,
    next_method: Value,
    pub(self) reusable_result: Field<Option<Gc<JsObject>>>,
}

struct Step {
    value: Value,
    done: bool,
}

impl JsAsyncFromSyncIterator {
    pub fn create(cx: &Cx, sync_iterator: Gc<JsObject>, next_method: Value) -> Gc<Self> {
        cx.allocate::<JsAsyncFromSyncIterator>(JsAsyncFromSyncIteratorInit {
            base: JsObjectInit::new(cx, cx.async_from_sync_iterator_shape()),
            sync_iterator,
            next_method,
            reusable_result: None,
        })
    }

    /// CreateAsyncFromSyncIterator(GetIteratorDirect(iterator))
    pub fn for_iterator(cx: &mut Cx, iterator: Gc<JsObject>) -> JsResult<Gc<Self>> {
        let next_method = iterator.get(cx, names::next)?;
        Ok(Self::create(cx, iterator, next_method))
    }

    /// CreateAsyncFromSyncIterator(GetIterator(iterable, sync))
    pub fn for_iterable(cx: &mut Cx, iterable: Value) -> JsResult<Gc<Self>> {
        let method = iterable.get(cx, symbols::iterator)?;
        if !method.is_callable() {
            return Err(cx.throw_type_error("iterable should have an iterator symbol"));
        }
        match cx.call(method, iterable, &[])?.as_object() {
            Some(iterator) => Self::for_iterator(cx, iterator),
            None => Err(cx.throw_type_error("iterator method should return an object")),
        }
    }

    pub fn sync_iterator(&self) -> Gc<JsObject> {
        self.sync_iterator
    }

    pub fn next_method(&self) -> Value {
        self.next_method
    }

    fn call_method(&self, cx: &mut Cx, method: Value, argument: Option<Value>) -> JsResult<Step> {
        if !method.is_callable() {
            return Err(cx.throw_type_error("Iterator method is not callable."));
        }
        let result = cx.call(method, self.sync_iterator.into(), argument.as_slice())?;
        let Some(result) = result.as_object() else {
            return Err(cx.throw_type_error(errors::ITERATOR_RESULT_NOT_OBJECT));
        };
        Ok(Step {
            done: result.get(cx, names::done)?.to_boolean(cx),
            value: result.get(cx, names::value)?,
        })
    }

    /// AsyncFromSyncIteratorContinuation. `unless_done` is the task for a step that is not the last.
    fn await_step(
        &self,
        cx: &mut Cx,
        target: Gc<JsObject>,
        step: JsResult<Step>,
        unless_done: InternalMicrotask,
    ) -> JsResult<()> {
        let Step { value, done } = match step {
            Ok(step) => step,
            Err(thrown) => {
                reject_target(cx, target, cx.catch_value(thrown)?);
                return Ok(());
            }
        };
        let task = if done {
            InternalMicrotask::AsyncFromSyncIteratorDone
        } else {
            unless_done
        };
        let reaction = InternalReaction::new(task, Some(target.cell()), self.into());
        match reaction.in_current_async_context(cx).await_value(cx, value) {
            Ok(()) => Ok(()),
            Err(thrown) => {
                let error = cx.catch_value(thrown)?;
                self.continue_with(cx, task, target, PromiseStatus::Rejected, error)
            }
        }
    }

    /// The value of a step has settled.
    #[inline(never)]
    pub(super) fn continue_with(
        &self,
        cx: &mut Cx,
        task: InternalMicrotask,
        target: Gc<JsObject>,
        status: PromiseStatus,
        value: Value,
    ) -> JsResult<()> {
        let cx = cx.in_realm_of(self);
        if status == PromiseStatus::Rejected {
            if task == InternalMicrotask::AsyncFromSyncIteratorContinueOrClose
                && let Err(thrown) = self.close(cx)
            {
                cx.catch(thrown)?;
            }
            reject_target(cx, target, value);
            return Ok(());
        }
        let done = task == InternalMicrotask::AsyncFromSyncIteratorDone;
        if let Some(promise) = target.downcast::<JsPromise>() {
            let result = cx.new_iterator_result(value, done);
            return promise.resolve(cx, result.into());
        }
        let (reusable, context) = (self.reusable_result(), cx.current_async_context());
        let result = answer(cx, target, value, done, reusable, context)?;
        if result != reusable {
            self.set_reusable_result(result);
        }
        Ok(())
    }

    /// IteratorClose
    fn close(&self, cx: &mut Cx) -> JsResult<()> {
        let (iterator, message) = (
            self.sync_iterator.as_value(),
            "Iterator return method is not callable.",
        );
        if let Some(method) = iterator.get_method(cx, names::r#return, message)?
            && !cx.call(method, iterator, &[])?.is_object()
        {
            return Err(cx.throw_type_error(errors::ITERATOR_RESULT_NOT_OBJECT));
        }
        Ok(())
    }

    /// `next`, for a promise or a `Coroutine`.
    fn request(&self, cx: &mut Cx, target: Gc<JsObject>, value: Option<Value>) -> JsResult<()> {
        let step = self.call_method(cx, self.next_method, value);
        let task = InternalMicrotask::AsyncFromSyncIteratorContinueOrClose;
        self.await_step(cx, target, step, task)
    }

    // The methods of the prototype. They fail only if execution is terminated.

    pub fn next(&self, cx: &mut Cx, value: Option<Value>) -> JsResult<Gc<JsPromise>> {
        let promise = JsPromise::new(cx);
        self.request(cx, promise.upcast(), value)?;
        Ok(promise)
    }

    pub fn r#return(&self, cx: &mut Cx, value: Option<Value>) -> JsResult<Gc<JsPromise>> {
        let step = match self.sync_iterator.get(cx, names::r#return) {
            Ok(method) if method.is_undefined_or_null() => {
                let result = cx.new_iterator_result(value.unwrap_or_default(), true);
                return JsPromise::new_resolved(cx, result.into());
            }
            method => method.and_then(|method| self.call_method(cx, method, value)),
        };
        let promise = JsPromise::new(cx);
        let task = InternalMicrotask::AsyncFromSyncIteratorContinue;
        self.await_step(cx, promise.upcast(), step, task)?;
        Ok(promise)
    }

    pub fn throw(&self, cx: &mut Cx, value: Option<Value>) -> JsResult<Gc<JsPromise>> {
        let step = match self.sync_iterator.get(cx, names::throw) {
            Ok(method) if method.is_undefined_or_null() => self.close(cx).and_then(|()| {
                Err(cx.throw_type_error("Iterator does not provide a throw method."))
            }),
            method => method.and_then(|method| self.call_method(cx, method, value)),
        };
        let promise = JsPromise::new(cx);
        let task = InternalMicrotask::AsyncFromSyncIteratorContinueOrClose;
        self.await_step(cx, promise.upcast(), step, task)?;
        Ok(promise)
    }
}

fn reject_target(cx: &Cx, target: Gc<JsObject>, reason: Value) {
    match target.downcast::<JsPromise>() {
        Some(promise) => promise.reject(cx, reason),
        None => answer_with_error(cx, target, reason),
    }
}
