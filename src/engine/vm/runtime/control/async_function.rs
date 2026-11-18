use core::mem::offset_of;

use bun_engine_bytecode::ResumeMode;
use bun_engine_macros::cell;

use super::async_iteration::requeue_early_answer;
use crate::{
    CallFrame, CellType, Coroutine, CoroutineInit, Cx, Gc, InternalMicrotask, InternalReaction,
    JsPromise, JsResult, PromiseStatus, Value,
};

/// An async function or a module that has come to an `await`. Scripts never see it.
#[cell(name = "AsyncFunctionGenerator", cell_type = AsyncFunctionGenerator,
    export = "engine_class_AsyncFunctionGenerator")]
pub struct JsAsyncFunctionGenerator {
    base: Coroutine,
    promise: Gc<JsPromise>,
}

impl JsAsyncFunctionGenerator {
    pub const OFFSET_OF_PROMISE: usize = offset_of!(JsAsyncFunctionGenerator, promise);

    #[inline(always)]
    pub(super) fn for_frame(cx: &Cx, frame: &CallFrame) -> JsResult<Gc<Self>> {
        let promise = JsPromise::new(cx);
        let base: CoroutineInit = Coroutine::init(cx, cx.async_function_generator_shape(), frame);
        Coroutine::allocate(cx, frame, JsAsyncFunctionGeneratorInit { base, promise })
    }

    /// `generator` is the operand of an instruction in the code that `frame` runs.
    #[inline(always)]
    pub fn ensure(cx: &Cx, frame: &CallFrame, generator: Option<Value>) -> JsResult<Gc<Self>> {
        match generator {
            Some(generator) => Ok(generator.cast()),
            None => Self::for_frame(cx, frame),
        }
    }

    #[inline(always)]
    pub fn promise(&self) -> Gc<JsPromise> {
        self.promise
    }

    pub fn is_of_module(&self) -> bool {
        self.callee().cell_type() == CellType::Callee
    }

    /// Arranges for the code to go on when `value` has settled. What it throws, the `await` throws.
    #[inline(always)]
    pub fn await_value(&self, cx: &mut Cx, value: Value) -> JsResult<()> {
        if value == cx.async_iteration_sentinel().into() {
            requeue_early_answer(cx, self);
            return Ok(());
        }
        InternalReaction::new(InternalMicrotask::AsyncFunctionResume, None, self.into())
            .in_current_async_context(cx)
            .await_value(cx, value)
    }

    #[inline(always)]
    pub(super) fn continue_with(
        &self,
        cx: &mut Cx,
        status: PromiseStatus,
        value: Value,
    ) -> JsResult<()> {
        match self.resume(cx, value, resume_mode_for(status)) {
            Ok(_) => Ok(()),
            // There was no room for the frame.
            Err(thrown) => self.promise.reject_with_caught(cx, thrown),
        }
    }
}

#[inline(always)]
pub(super) fn resume_mode_for(status: PromiseStatus) -> ResumeMode {
    match status {
        PromiseStatus::Rejected => ResumeMode::Throw,
        _ => ResumeMode::Normal,
    }
}

/// Fails only if execution is terminated.
#[inline(always)]
pub fn async_return(
    cx: &mut Cx,
    generator: Option<Value>,
    value: Value,
) -> JsResult<Gc<JsPromise>> {
    let Some(generator) = generator else {
        return JsPromise::new_resolved(cx, value);
    };
    let generator = generator.cast::<JsAsyncFunctionGenerator>();
    generator.complete();
    generator.promise.resolve(cx, value)?;
    Ok(generator.promise)
}

pub fn async_throw(cx: &Cx, generator: Option<Value>, reason: Value) -> Gc<JsPromise> {
    let Some(generator) = generator else {
        return JsPromise::rejected(cx, reason);
    };
    let generator = generator.cast::<JsAsyncFunctionGenerator>();
    generator.complete();
    generator.promise.reject(cx, reason);
    generator.promise
}
