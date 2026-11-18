//! What the microtasks do. What one throws is reported as uncaught.

use bun_engine_text::names;

use super::async_context::AsyncContextScope;
use super::async_iteration::run_answer;
use super::promise::Resolution;
use super::reaction::HandlerContext;
use super::resolving::create_resolving_functions;
use crate::runtime::modules;
use crate::{
    Cx, Gc, InternalFieldTuple, InternalMicrotask, InternalReaction, JsAsyncFromSyncIterator,
    JsAsyncFunctionGenerator, JsAsyncGenerator, JsPromise, JsResult, PromiseCombinator,
    PromiseReaction, PromiseStatus, QueuedTask, Value, ValueVec,
};

/// The jobs have frames of their own, so nothing that they leave on the stack outlives them.
#[inline(always)]
pub(super) fn run(cx: &mut Cx, task: &QueuedTask) -> JsResult<()> {
    use InternalMicrotask::*;
    let (job, payload) = (task.job(), task.payload());
    let [cell, value, context, last] = task.arguments.map(Option::unwrap_or_default);
    let status = PromiseStatus::from_payload(payload);
    if job == AsyncFunctionResume {
        return resume_async_function(cx, status, value, context, last);
    }
    if job == PromiseReaction {
        return react(cx, payload, cell, value, context, last);
    }
    run_less_common(cx, task)
}

#[inline(never)]
pub(super) fn run_out_of_line(cx: &mut Cx, task: &QueuedTask) -> JsResult<()> {
    run(cx, task)
}

#[inline(never)]
fn run_less_common(cx: &mut Cx, task: &QueuedTask) -> JsResult<()> {
    use InternalMicrotask::*;
    let (job, payload) = (task.job(), task.payload());
    let arguments = task.arguments.map(Option::unwrap_or_default);
    let [cell, value, context, last] = arguments;
    let status = PromiseStatus::from_payload(payload);
    match job {
        AsyncFunctionResume | PromiseReaction => unreachable!(),
        CallInAsyncContext => call_in_async_context(cx, task),
        CallWithArguments => call_with_arguments(cx, task),
        PromiseResolveWithoutHandler => settle(cx, cell, status, value),
        PromiseResolveThenable => {
            let _scope = AsyncContextScope::enter(cx, last);
            follow_thenable(cx, cell, value, context)
        }
        PromiseResolveNativeThenable => resolve_native_thenable(cx, cell.cast(), value, context),
        PromiseReactToNativeThenable => {
            let reaction = InternalReaction {
                task: InternalMicrotask::from_bits(payload),
                cell,
                context,
                async_context: last,
            };
            react_to_native_thenable(cx, value.cast(), &reaction)
        }
        PromiseRaceElement => resolve_or_reject(cx, cell.cast(), status, value),
        PromiseAllElement | PromiseAllSettledElement | PromiseAnyElement => {
            (cell.cast::<PromiseCombinator>()).did_settle(cx, context, status, value)
        }
        PromiseFinally => {
            let _scope = AsyncContextScope::enter(cx, last);
            call_finally(cx, cell.cast(), context, status, value)
        }
        PromiseFinallyFulfill | PromiseFinallyReject if status == PromiseStatus::Rejected => {
            settle(cx, cell, status, value)
        }
        PromiseFinallyFulfill => settle(cx, cell, PromiseStatus::Fulfilled, context),
        PromiseFinallyReject => settle(cx, cell, PromiseStatus::Rejected, context),
        AsyncGeneratorResume
        | AsyncGeneratorYield
        | AsyncGeneratorResumeWithReturn
        | AsyncGeneratorAwaitReturn => {
            let _scope = AsyncContextScope::enter(cx, last);
            (context.cast::<JsAsyncGenerator>()).continue_with(cx, job, status, value)
        }
        AsyncIteratorAnswer => run_answer(cx, payload, arguments),
        AsyncFromSyncIteratorContinue
        | AsyncFromSyncIteratorContinueOrClose
        | AsyncFromSyncIteratorDone => {
            let _scope = AsyncContextScope::enter(cx, last);
            let iterator = context.cast::<JsAsyncFromSyncIterator>();
            iterator.continue_with(cx, job, cell.cast(), status, value)
        }
        PromiseFulfillWithoutHandler => {
            let promise = cell.cast::<JsPromise>();
            match status {
                PromiseStatus::Rejected => promise.reject_promise(cx, value),
                _ => promise.fulfill_promise(cx, value),
            }
            Ok(())
        }
        _ => modules::run_microtask(cx, job, status, cell, value, context),
    }
}

#[inline(never)]
fn resume_async_function(
    cx: &mut Cx,
    status: PromiseStatus,
    value: Value,
    generator: Value,
    async_context: Value,
) -> JsResult<()> {
    let _scope = AsyncContextScope::enter(cx, async_context);
    (generator.cast::<JsAsyncFunctionGenerator>()).continue_with(cx, status, value)
}

#[inline(never)]
fn react(
    cx: &mut Cx,
    payload: u8,
    first: Value,
    value: Value,
    handler: Value,
    last: Value,
) -> JsResult<()> {
    let (result, argument, async_context) = match HandlerContext::from_payload(payload) {
        HandlerContext::Async => (first, None, last),
        HandlerContext::Argument => (first, Some(last), Value::UNDEFINED),
        HandlerContext::BothWithoutResult => (Value::UNDEFINED, Some(last), first),
        HandlerContext::Both => {
            let both = last.cast::<InternalFieldTuple>();
            (first, Some(both.fields().at(0)), both.fields().at(1))
        }
    };
    let _scope = AsyncContextScope::enter(cx, async_context);
    let outcome = match argument {
        None => cx.call(handler, Value::UNDEFINED, &[value]),
        Some(argument) => cx.call(handler, Value::UNDEFINED, &[value, argument]),
    };
    match outcome {
        Ok(value) => settle(cx, result, PromiseStatus::Fulfilled, value),
        Err(thrown) if result.is_undefined_or_null() => Err(thrown),
        Err(thrown) => {
            let error = cx.catch_value(thrown)?;
            settle(cx, result, PromiseStatus::Rejected, error)
        }
    }
}

/// `target` is as the `result` of `perform_then`.
#[inline(always)]
fn settle(cx: &mut Cx, target: Value, status: PromiseStatus, value: Value) -> JsResult<()> {
    let Some(promise) = target.downcast::<JsPromise>() else {
        return settle_capability(cx, target, status, value);
    };
    if status == PromiseStatus::Rejected {
        promise.reject_promise(cx, value);
        return Ok(());
    }
    if !value.is_object() {
        promise.fulfill_promise(cx, value);
        return Ok(());
    }
    promise.resolve_promise(cx.in_realm(promise.global()), value)
}

#[inline(never)]
fn settle_capability(
    cx: &mut Cx,
    capability: Value,
    status: PromiseStatus,
    value: Value,
) -> JsResult<()> {
    let Some(capability) = capability.as_object() else {
        return Ok(());
    };
    let name = match status {
        PromiseStatus::Rejected => names::reject,
        _ => names::resolve,
    };
    let function = capability.get(cx, name)?;
    cx.call(function, Value::UNDEFINED, &[value]).map(drop)
}

fn resolve_or_reject(
    cx: &mut Cx,
    promise: Gc<JsPromise>,
    status: PromiseStatus,
    value: Value,
) -> JsResult<()> {
    if status == PromiseStatus::Rejected {
        promise.reject(cx, value);
        return Ok(());
    }
    promise.resolve(cx.in_realm(promise.global()), value)
}

/// `call_then` gets the resolving functions.
fn follow(
    cx: &mut Cx,
    target: Value,
    call_then: impl FnOnce(&mut Cx, [Value; 2]) -> JsResult<()>,
) -> JsResult<()> {
    let functions = create_resolving_functions(cx, target);
    if let Err(thrown) = call_then(cx, functions) {
        let error = cx.catch_value(thrown)?;
        cx.call(functions[1], Value::UNDEFINED, &[error])?;
    }
    Ok(())
}

fn follow_thenable(cx: &mut Cx, thenable: Value, then: Value, target: Value) -> JsResult<()> {
    follow(cx, target, |cx, functions| {
        cx.call(then, thenable, &functions).map(drop)
    })
}

#[inline(never)]
fn follow_with_species(cx: &mut Cx, promise: Gc<JsPromise>, target: Value) -> JsResult<()> {
    follow(cx, target, |cx, [resolve, reject]| {
        promise.then_with_species(cx, resolve, reject).map(drop)
    })
}

fn resolve_native_thenable(
    cx: &mut Cx,
    promise: Gc<JsPromise>,
    target: Value,
    async_context: Value,
) -> JsResult<()> {
    if !promise.has_pristine_species(cx) {
        let _scope = AsyncContextScope::enter(cx, async_context);
        return follow_with_species(cx, promise, target);
    }
    let task = InternalMicrotask::PromiseResolveWithoutHandler;
    let reaction = InternalReaction::new(task, target.as_cell(), Value::UNDEFINED);
    promise.react(cx, reaction);
    Ok(())
}

fn react_to_native_thenable(
    cx: &mut Cx,
    promise: Gc<JsPromise>,
    reaction: &InternalReaction,
) -> JsResult<()> {
    if !promise.has_pristine_species(cx) {
        let _scope = AsyncContextScope::enter(cx, reaction.async_context);
        let target = PromiseReaction::internal(cx, *reaction, None);
        return follow_with_species(cx, promise, target.into());
    }
    promise.react(cx, *reaction);
    Ok(())
}

fn call_finally(
    cx: &mut Cx,
    result: Gc<JsPromise>,
    on_finally: Value,
    status: PromiseStatus,
    value: Value,
) -> JsResult<()> {
    let cx = cx.in_realm(result.global());
    let returned = match cx.call(on_finally, Value::UNDEFINED, &[]) {
        Ok(returned) => returned,
        Err(thrown) => {
            result.reject_promise(cx, cx.catch_value(thrown)?);
            return Ok(());
        }
    };
    let Some(returned) = returned.as_object() else {
        return settle(cx, result.into(), status, value);
    };
    let task = match status {
        PromiseStatus::Rejected => InternalMicrotask::PromiseFinallyReject,
        _ => InternalMicrotask::PromiseFinallyFulfill,
    };
    let reaction = InternalReaction::new(task, Some(result.cell()), value);
    if let Some(promise) = returned.downcast::<JsPromise>()
        && promise.global() == cx.global()
        && promise.has_pristine_then(cx)
    {
        promise.react(cx, reaction);
        return Ok(());
    }
    match Resolution::of(cx, returned)? {
        Resolution::Settled(PromiseStatus::Rejected, error) => {
            settle(cx, result.into(), PromiseStatus::Rejected, error)
        }
        Resolution::Settled(..) => settle(cx, result.into(), status, value),
        Resolution::Thenable(then) => {
            let target = PromiseReaction::internal(cx, reaction, None);
            follow_thenable(cx, returned.into(), then, target.into())
        }
    }
}

fn call_in_async_context(cx: &mut Cx, task: &QueuedTask) -> JsResult<()> {
    let [function, async_context, first, second] = task.arguments;
    let Some(function) = function.filter(|it| it.is_callable()) else {
        return Ok(());
    };
    let scope = AsyncContextScope::enter(cx, async_context.unwrap_or_default());
    let result = match (first, second) {
        (first, Some(second)) => {
            let arguments = [first.unwrap_or_default(), second];
            cx.call(function, Value::UNDEFINED, &arguments)
        }
        (first, None) => cx.call(function, Value::UNDEFINED, first.as_slice()),
    };
    drop(scope);
    match (result, cx.embedder().microtasks.report_error.get()) {
        (Err(thrown), Some(report)) => {
            let exception = cx.catch(thrown)?;
            report(cx, exception);
            cx.check_exception()
        }
        (result, _) => result.map(drop),
    }
}

fn call_with_arguments(cx: &mut Cx, task: &QueuedTask) -> JsResult<()> {
    let [function, rest @ ..] = task.arguments;
    let mut arguments = ValueVec::new();
    rest.into_iter().flatten().for_each(|it| arguments.push(it));
    cx.call(function.unwrap_or_default(), Value::UNDEFINED, &arguments)
        .map(drop)
}
