//! CreateDynamicFunction of ECMA-262.

use bun_engine_text::StringBuilder;

use super::eval::generated_source;
use crate::{
    Cx, FunctionExecutable, Gc, JsFunction, JsObject, JsResult, Value, origin_of_caller,
    throw_compile_error,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DynamicFunctionKind {
    Normal,
    Generator,
    Async,
    AsyncGenerator,
}

/// `arguments` are the parameters and the body. `new_target` is undefined in a call without `new`.
pub fn create_dynamic_function(
    cx: &mut Cx,
    kind: DynamicFunctionKind,
    constructor: &JsObject,
    new_target: Value,
    arguments: &[Value],
) -> JsResult<Gc<JsFunction>> {
    let mut text = StringBuilder::new();
    text.push_ascii(match kind {
        DynamicFunctionKind::Normal => "function anonymous(",
        DynamicFunctionKind::Generator => "function* anonymous(",
        DynamicFunctionKind::Async => "async function anonymous(",
        DynamicFunctionKind::AsyncGenerator => "async function* anonymous(",
    });
    let (body, parameters) = arguments.split_last().unzip();
    let parameters = parameters.unwrap_or_default();
    for (index, parameter) in parameters.iter().enumerate() {
        if index != 0 {
            text.push_latin1(b',');
        }
        text.push_text(parameter.to_string_view(cx)?.text());
    }
    text.push_ascii("\n)");
    let parameters_end = (!parameters.is_empty()).then_some(text.len() as u32);
    text.push_ascii(" {\n");
    if let Some(body) = body {
        text.push_text(body.to_string_view(cx)?.text());
    }
    text.push_ascii("\n}");
    cx.ensure_can_compile_strings()?;
    let text = cx.or_out_of_memory(text.finish())?;

    let provider = generated_source(text, origin_of_caller(cx).as_deref()).provider;
    let function = (cx.compile_single_function(&provider, parameters_end))
        .map_err(|error| throw_compile_error(cx, &error, &provider))?;
    let executable = FunctionExecutable::create(cx, provider, None, function);
    let shape = cx.shape_from_new_target(new_target, constructor, |global, vm| {
        JsFunction::shape_for(vm, global, &executable)
    })?;
    // SAFETY: compiled for the global scope.
    Ok(unsafe { JsFunction::create_with_shape(cx, executable, cx.global().cell(), shape) })
}
