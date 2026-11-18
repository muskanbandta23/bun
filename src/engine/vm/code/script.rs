//! ScriptEvaluation of ECMA-262.

use std::rc::Rc;

use bun_engine_bytecode::{Declarations, Script};
use bun_engine_text::PropertyName;

use crate::realm::Declarer;
use crate::scope::static_scope_of;
use crate::{
    Attributes, Cx, ErrorType, Gc, JsCell, JsResult, ProgramExecutable, SourceCode, Thrown, Value,
    errors, throw_compile_error,
};

/// In the realm of `this`, which is the global object of `cx` if it is undefined or null.
pub fn evaluate_script(cx: &mut Cx, source: &SourceCode, this: Value) -> JsResult<Value> {
    let this = this.to_sloppy_this(cx);
    let cx = cx.in_realm_of(&this);
    evaluate_script_in_scope(cx, source, cx.global().cell(), this.into())
}

/// `scope` is one of the realm of `cx`.
pub fn evaluate_script_in_scope(
    cx: &mut Cx,
    source: &SourceCode,
    scope: Gc<JsCell>,
    this: Value,
) -> JsResult<Value> {
    cx.check_stack()?;
    match cx.compile_script(&source.provider, static_scope_of(scope).as_ref()) {
        // SAFETY: just compiled for it.
        Ok(script) => unsafe { evaluate_compiled_script(cx, source, script, scope, this) },
        Err(error) => {
            let thrown = throw_compile_error(cx, &error, &source.provider);
            cx.notify_debugger_of_exception();
            Err(thrown)
        }
    }
}

/// # Safety
/// `script` was compiled from the text of `source` against the static scope of `scope`.
pub unsafe fn evaluate_compiled_script(
    cx: &mut Cx,
    source: &SourceCode,
    script: Rc<Script>,
    scope: Gc<JsCell>,
    this: Value,
) -> JsResult<Value> {
    let executable = ProgramExecutable::create(cx, source, script);
    declare_globals(cx, &executable.unit().declarations)
        .inspect_err(|_| cx.notify_debugger_of_exception())?;
    let global = cx.global();
    if global.shape().is_uncacheable_dictionary() {
        global.flatten_dictionary(cx);
    }
    let result = cx.execute(executable.upcast(), scope, this);
    executable.release_code();
    result
}

#[cold]
fn throw_duplicate(cx: &Cx, name: PropertyName<'_>) -> Thrown {
    let (before, fallback) = (
        "Can't create duplicate variable: '",
        "Can't create duplicate variable",
    );
    errors::throw_naming(cx, ErrorType::SyntaxError, before, name, "'", fallback)
}

/// GlobalDeclarationInstantiation. The code makes the functions.
fn declare_globals(cx: &mut Cx, declarations: &Declarations) -> JsResult<()> {
    let _defer = cx.defer_termination();
    let global = cx.global();
    let Declarations {
        variables,
        functions,
        hoisted_functions,
        lets,
        constants,
    } = declarations;
    for name in lets.iter().chain(constants) {
        if global.lexicals().find(name.name()).is_some() {
            if cx.allows_redeclaration() {
                continue;
            }
            return Err(throw_duplicate(cx, name.name()));
        }
        let property = global.get_own_property_descriptor(cx, name.name())?;
        if property.is_some_and(|it| !it.configurable()) {
            return Err(errors::throw_naming(
                cx,
                ErrorType::SyntaxError,
                "Can't create duplicate variable that shadows a global property: '",
                name.name(),
                "'",
                "Can't create duplicate variable that shadows a global property",
            ));
        }
    }
    for name in functions.iter().chain(variables) {
        if global.lexicals().find(name.name()).is_some() {
            return Err(throw_duplicate(cx, name.name()));
        }
    }
    global.check_can_declare(cx, declarations)?;
    for name in hoisted_functions {
        if global.lexicals().find(name.name()).is_none()
            && global.can_declare_variable(cx, name.name())?
        {
            global.declare_variable(cx, name.name(), Declarer::Script)?;
        }
    }
    global.declare_functions_and_variables(cx, declarations, Declarer::Script)?;
    for (names, attributes) in [(lets, Attributes::NONE), (constants, Attributes::READ_ONLY)] {
        for name in names {
            if !cx.allows_redeclaration() || global.lexicals().find(name.name()).is_none() {
                global.add_lexical(cx, name.name(), attributes);
            }
        }
    }
    Ok(())
}
