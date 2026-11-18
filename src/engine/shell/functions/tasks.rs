//! Time, timers and microtasks.

use core::time::Duration;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime};

use bun_engine_vm::{JsFunction, WorkKind};

use super::return_undefined;
use crate::prelude::*;

pub(super) const FUNCTIONS: Functions = &[
    ("preciseTime", precise_time, 0),
    ("sleepSeconds", sleep_seconds, 1),
    ("flashHeapAccess", sleep_milliseconds, 0),
    ("dropAllLocks", return_undefined, 1),
    ("setTimeout", set_timeout, 2),
    ("drainMicrotasks", drain_microtasks, 0),
    ("asyncTestStart", async_test_start, 1),
    ("asyncTestPassed", async_test_passed, 1),
];

pub(super) const PERFORMANCE: Functions = &[
    ("now", monotonic_now, 0),
    ("mark", return_undefined, 1),
    ("measure", return_undefined, 2),
];

#[native_abi]
fn precise_time(_: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    let since_epoch = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);
    Ok(since_epoch.unwrap_or_default().as_secs_f64().into())
}

/// In milliseconds.
#[native_abi]
pub(super) fn monotonic_now(_: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    let elapsed = ORIGIN.get_or_init(Instant::now).elapsed();
    Ok((elapsed.as_secs_f64() * 1000.0).into())
}

fn duration(seconds: f64) -> Duration {
    Duration::try_from_secs_f64(seconds.min(1e9)).unwrap_or_default()
}

#[native_abi]
fn sleep_seconds(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    std::thread::sleep(duration(frame.argument(0).to_number(cx)?));
    Ok(Value::UNDEFINED)
}

#[native_abi]
pub(super) fn sleep_milliseconds(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    std::thread::sleep(duration(frame.argument(0).to_number(cx)? / 1000.0));
    Ok(Value::UNDEFINED)
}

#[native_abi]
fn set_timeout(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let Some(callback) = frame.argument(0).downcast::<JsFunction>() else {
        return Err(cx.throw_type_error("First argument is not a JS function"));
    };
    let delay = duration(frame.argument(1).as_number().unwrap_or(0.0) / 1000.0);
    let ticket = cx.add_pending_work(WorkKind::AtSomePoint, callback.upcast(), &[]);
    cx.timer_queue().start(delay, move |_| {
        ticket.schedule(|cx, ticket| {
            let callback = ticket.target(cx).map_or_default(Into::into);
            cx.call(callback, Value::UNDEFINED, &[]).map(|_| ())
        });
    });
    Ok(Value::UNDEFINED)
}

#[native_abi]
fn drain_microtasks(cx: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    cx.drain_microtasks().map(|()| Value::UNDEFINED)
}

#[native_abi]
fn async_test_start(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let Some(count) = frame.argument(0).as_index_int32() else {
        let message = "Expected first argument to be a uint32";
        return Err(cx.throw_error(ErrorType::Error, message));
    };
    (State::of(cx).expected_async_passes).update(|expected| expected + count);
    Ok(Value::UNDEFINED)
}

#[native_abi]
fn async_test_passed(cx: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    State::of(cx).async_passes.update(|passes| passes + 1);
    Ok(Value::UNDEFINED)
}
