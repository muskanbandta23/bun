//! PerformEval of ECMA-262.

use core::hash::{Hash, Hasher};
use std::rc::Rc;

use bun_engine_bytecode::{Declarations, EvalContext, StaticScope};
use bun_engine_text::StringRef;

use crate::realm::Declarer;
use crate::scope::{VariableScope, static_scope_of};
use crate::{
    CodeBlock, Cx, ErrorType, EvalExecutable, Gc, JsCell, JsResult, SourceCode, SourceProvider,
    SourceProviderRef, SourceType, StackVisitor, Value, Vm, create_compile_error, errors,
};

#[derive(Clone)]
pub(super) struct EvalKey {
    text: StringRef,
    scope: Option<Rc<StaticScope>>,
    context: EvalContext,
}

impl PartialEq for EvalKey {
    fn eq(&self, other: &EvalKey) -> bool {
        self.scope.as_ref().map(Rc::as_ptr) == other.scope.as_ref().map(Rc::as_ptr)
            && self.context == other.context
            && self.text == other.text
    }
}

impl Eq for EvalKey {}

impl Hash for EvalKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.text.hash(state);
    }
}

// SAFETY: text and what is known of scopes before they exist.
unsafe impl crate::heap::PlainKey for EvalKey {}

const MAX_CACHED_EVALS: usize = 64;

/// What text that the running native function turns into code has for an origin.
pub fn origin_of_caller(vm: &Vm) -> Option<SourceProviderRef> {
    StackVisitor::frames(vm)
        .skip(1)
        .take_while(|frame| frame.callee.is_some())
        .filter_map(|frame| Some(frame.code_block?.executable()))
        .find(|executable| !executable.is_private_builtin())
        .map(|executable| SourceProviderRef::retain(executable.provider().origin()))
}

pub(super) fn generated_source(text: StringRef, origin: Option<&SourceProvider>) -> SourceCode {
    let provider = SourceProvider::build(text, None, SourceType::Program);
    SourceCode::whole(provider.origin(origin).finish())
}

impl Cx {
    /// HostEnsureCanCompileStrings
    pub fn ensure_can_compile_strings(&self) -> JsResult<()> {
        let message = self.global().eval_disabled_message().borrow().clone();
        match message {
            Some(message) => Err(self.throw_error(ErrorType::EvalError, message)),
            None => Ok(()),
        }
    }
}

/// `scope` is the one that `caller` is in where it calls `eval`, and `context` is what the code there allows.
pub fn direct_eval(
    cx: &mut Cx,
    caller: &CodeBlock,
    program: Value,
    scope: Gc<JsCell>,
    context: EvalContext,
) -> JsResult<Value> {
    let Some(program) = program.as_string() else {
        return Ok(program);
    };
    cx.ensure_can_compile_strings()?;
    let key = EvalKey {
        text: program.to_string_ref(cx)?,
        scope: static_scope_of(scope),
        context,
    };
    let caller = caller.executable();
    let executable = match caller.evals().get(&key) {
        Some(executable) => executable,
        None => {
            let source = generated_source(key.text.clone(), Some(caller.provider()));
            let eval = cx.compile_direct_eval(&source.provider, context, key.scope.clone());
            let eval = eval.map_err(|error| cx.throw(create_compile_error(cx, &error).into()))?;
            let executable = EvalExecutable::create(cx, &source, eval);
            if executable.unit().is_reusable && caller.evals().len() < MAX_CACHED_EVALS {
                caller.evals().insert(key, executable);
            }
            executable
        }
    };
    execute(cx, executable, scope)
}

/// What `eval` does when it is called as any function is.
pub fn indirect_eval(cx: &mut Cx, program: Value) -> JsResult<Value> {
    let Some(program) = program.as_string() else {
        return Ok(program);
    };
    let text = program.to_string_ref(cx)?;
    cx.ensure_can_compile_strings()?;
    let executable = prepare_indirect_eval(cx, text, origin_of_caller(cx).as_deref())?;
    execute_indirect_eval(cx, executable)
}

/// The first half of `indirect_eval`, for a caller that tells errors in the text from those that it throws.
pub fn prepare_indirect_eval(
    cx: &Cx,
    text: StringRef,
    origin: Option<&SourceProvider>,
) -> JsResult<Gc<EvalExecutable>> {
    let source = generated_source(text, origin);
    match cx.compile_indirect_eval(&source.provider) {
        Ok(eval) => Ok(EvalExecutable::create(cx, &source, eval)),
        Err(error) => Err(cx.throw(create_compile_error(cx, &error).into())),
    }
}

/// Once, in the realm that it was prepared in.
pub fn execute_indirect_eval(cx: &mut Cx, executable: Gc<EvalExecutable>) -> JsResult<Value> {
    execute(cx, executable, cx.global().cell())
}

fn execute(cx: &mut Cx, executable: Gc<EvalExecutable>, scope: Gc<JsCell>) -> JsResult<Value> {
    cx.check_stack()?;
    declare(cx, &executable.unit().declarations, scope)?;
    cx.execute(executable.upcast(), scope, cx.global_this().into())
}

/// EvalDeclarationInstantiation. Strict code declares nothing outside itself, and the code makes the functions.
fn declare(cx: &mut Cx, declarations: &Declarations, scope: Gc<JsCell>) -> JsResult<()> {
    let Declarations {
        variables,
        functions,
        hoisted_functions,
        ..
    } = declarations;
    if variables.is_empty() && functions.is_empty() && hoisted_functions.is_empty() {
        return Ok(());
    }
    let target = VariableScope::around(scope);
    for name in variables.iter().chain(functions) {
        if target.is_hidden(name.name()) {
            return Err(errors::throw_naming(
                cx,
                ErrorType::SyntaxError,
                "Can't create duplicate variable in eval: '",
                name.name(),
                "'",
                "Can't create duplicate variable in eval",
            ));
        }
    }
    let hoisted = (hoisted_functions.iter()).filter(|name| !target.is_hidden(name.name()));
    if let Some(environment) = target.environment() {
        for name in hoisted.chain(functions).chain(variables) {
            environment.declare_eval_variable(cx, name.name());
        }
        return Ok(());
    }
    let global = cx.global();
    global.check_can_declare(cx, declarations)?;
    for name in hoisted {
        if global.can_declare_variable(cx, name.name())? {
            global.declare_variable(cx, name.name(), Declarer::Eval)?;
        }
    }
    global.declare_functions_and_variables(cx, declarations, Declarer::Eval)
}
