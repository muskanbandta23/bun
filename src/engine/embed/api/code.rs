use std::rc::Rc;

use bun_engine_bytecode::{FunctionKind, Opcode, Position, Visibility};
use bun_engine_macros::{FieldOffsets, export};
use bun_engine_text::{PropertyName, StringRef};
use bun_engine_vm::{
    AsciiLiteral, BuiltinOutline, CodeBlock, CompileError, CompileErrorKind, CompileErrorRecovery,
    Cx, ErrorInstance, Executable, FunctionExecutable, Gc, JsResult, NativeExecutable,
    ScriptExecutable, SourceCode, SourceProvider, StaticBuiltin, UnlinkedFunction, Value, Vm,
    create_compile_error, describe_builtin, options,
};

use crate::abi::returned_by_value;

returned_by_value! {
    Visibility = Visibility::Public,
}

#[derive(FieldOffsets)]
#[repr(C)]
pub(crate) struct ParserError {
    message: Option<StringRef>,
    token_start: u32,
    token_end: u32,
    line: i32,
    is_valid: bool,
    kind: CompileErrorKind,
    recovery: CompileErrorRecovery,
}

impl ParserError {
    #[cold]
    pub(super) fn set(&mut self, error: &CompileError, provider: &SourceProvider) {
        *self = ParserError {
            message: Some(error.message.clone()),
            token_start: error.token.start,
            token_end: error.token.end,
            line: match error.kind {
                CompileErrorKind::Syntax => provider.position_in_document(error.offset).line as i32,
                _ => -1,
            },
            is_valid: true,
            kind: error.kind,
            recovery: error.recovery,
        };
    }

    /// Notes the error, if there is one.
    pub(super) fn take<T>(
        &mut self,
        result: Result<T, CompileError>,
        provider: &SourceProvider,
    ) -> Option<T> {
        result.map_err(|error| self.set(&error, provider)).ok()
    }
}

#[export]
fn parser_error_to_object(
    error: &ParserError,
    cx: &mut Cx,
    provider: &SourceProvider,
    line: u32,
) -> Gc<ErrorInstance> {
    let error = CompileError {
        kind: error.kind,
        recovery: error.recovery,
        message: error.message.clone().unwrap_or_default(),
        token: 0..0,
        offset: 0,
    };
    let instance = create_compile_error(cx, &error);
    if error.kind == CompileErrorKind::Syntax {
        instance.locate_in_source(cx, line, provider.url());
    }
    instance
}

#[export]
fn check_script_syntax(vm: &Vm, provider: &SourceProvider, error: &mut ParserError) -> bool {
    (error.take(vm.check_script_syntax(provider), provider)).is_some()
}

#[export]
fn evaluate_script(cx: &mut Cx, source: &SourceCode, this: Value) -> JsResult<Value> {
    bun_engine_vm::evaluate_script(cx, source, this)
}

#[export]
fn global_instrumentation(cx: &Cx) -> u8 {
    cx.global().instrumentation(cx).bits()
}

#[export]
fn executable_visibility(executable: &Executable) -> Visibility {
    executable.visibility()
}

#[export]
fn native_executable_set_is_named_as_bound(executable: &NativeExecutable) {
    executable.is_named_as_bound().set(true);
}

#[export]
fn executable_source_provider(executable: &ScriptExecutable) -> *const SourceProvider {
    &raw const *executable.code_provider()
}

#[export]
fn executable_source_range(executable: &ScriptExecutable, start: &mut u32, end: &mut u32) {
    let range = executable.code_range();
    (*start, *end) = (range.start, range.end);
}

#[export]
fn executable_set_never_inline(executable: &ScriptExecutable, value: bool) {
    executable.never_inline().set(value);
}

#[export]
fn executable_set_never_fully_optimize(executable: &ScriptExecutable, value: bool) {
    executable.never_fully_optimize().set(value);
}

#[export]
fn function_executable_declared_name(executable: &FunctionExecutable) -> StringRef {
    executable.declared_name()
}

#[export]
fn function_executable_name(executable: &FunctionExecutable) -> StringRef {
    executable.display_name()
}

#[export]
fn function_executable_length(executable: &FunctionExecutable) -> u32 {
    executable.info().length()
}

#[export]
fn function_executable_kind(executable: &FunctionExecutable) -> u8 {
    executable.info().kind() as u8
}

#[export]
fn function_executable_origin(executable: &FunctionExecutable) -> u8 {
    executable.info().origin().bits()
}

#[export]
fn function_executable_is_async(executable: &FunctionExecutable) -> bool {
    executable.info().is_async()
}

#[export]
fn function_executable_is_generator(executable: &FunctionExecutable) -> bool {
    executable.info().is_generator()
}

#[export]
fn function_executable_can_suspend(executable: &FunctionExecutable) -> bool {
    executable.info().is_generator() || executable.info().awaits()
}

/// Of a constructor, the class. Returns the text that it is in.
#[export]
fn function_executable_text_range(
    executable: &FunctionExecutable,
    start: &mut u32,
    end: &mut u32,
) -> *const SourceProvider {
    let range = executable.info().text_range();
    (*start, *end) = (range.start, range.end);
    &raw const **executable.provider()
}

#[export]
fn code_block_kind(code_block: &CodeBlock) -> u8 {
    code_block.body().kind() as u8
}

#[export]
fn code_block_flags(code_block: &CodeBlock) -> u8 {
    code_block.body().flags().bits()
}

#[export]
fn code_block_position(code_block: &CodeBlock, bytecode_offset: u32, position: &mut Position) {
    *position = code_block.position(bytecode_offset);
}

#[export]
fn code_block_constructs_at(code_block: &CodeBlock, bytecode_offset: u32) -> bool {
    matches!(
        code_block.opcode_at(bytecode_offset),
        Some(
            Opcode::Construct
                | Opcode::ConstructVarargs
                | Opcode::SuperConstruct
                | Opcode::SuperConstructVarargs
        )
    )
}

#[export]
fn code_block_baseline(code_block: &CodeBlock) -> Gc<CodeBlock> {
    code_block.baseline()
}

#[export]
fn code_block_reoptimizations(code_block: &CodeBlock) -> u32 {
    code_block.reoptimizations()
}

/// Very many if nothing optimizes, so that a loop that waits for it ends.
#[export]
fn code_block_optimization_count(code_block: &CodeBlock) -> u32 {
    let options = options();
    if !(options.use_jit && options.use_baseline_jit && options.use_quick_optimizer) {
        return 1_000_000;
    }
    let installed = (code_block.executable()).code_block(code_block.specialization());
    code_block.reoptimizations() + u32::from(installed.is_some_and(|it| it.is_optimized()))
}

#[export]
fn code_block_optimize_next_invocation(code_block: &CodeBlock) {
    code_block.execution_counter().set(0);
}

/// Panics if the text is no function in parentheses.
#[export]
fn builtin_outline_of(text: &[u8], outline: &mut BuiltinOutline) {
    *outline = BuiltinOutline::of(text);
}

/// With one reference.
#[export]
fn unlinked_function_create_builtin(
    source: &SourceCode,
    outline: &BuiltinOutline,
    name: PropertyName<'_>,
    visibility: Visibility,
) -> *const UnlinkedFunction {
    let (name, kind) = (name.to_identifier(), FunctionKind::Method);
    Rc::into_raw(Rc::new(UnlinkedFunction {
        provider: source.provider.clone(),
        function: describe_builtin(source, outline, name, kind, visibility),
    }))
}

#[export]
unsafe fn unlinked_function_retain(function: *const UnlinkedFunction) {
    // SAFETY: C++ holds a reference.
    unsafe { Rc::increment_strong_count(function) }
}

#[export]
unsafe fn unlinked_function_release(function: *const UnlinkedFunction) {
    // SAFETY: as above.
    unsafe { Rc::decrement_strong_count(function) }
}

#[export]
fn unlinked_function_source_provider(function: &UnlinkedFunction) -> &SourceProvider {
    &function.provider
}

#[export]
fn unlinked_function_link(function: &UnlinkedFunction, vm: &Vm) -> Gc<FunctionExecutable> {
    function.link(vm)
}

#[export]
unsafe fn static_builtin_link(
    vm: &Vm,
    text: *const u8,
    outline: &BuiltinOutline,
    name: AsciiLiteral,
    visibility: Visibility,
    is_constructor: bool,
) -> Gc<FunctionExecutable> {
    vm.link_static_builtin(StaticBuiltin {
        // SAFETY: in static memory, and at least that long.
        text: unsafe { core::slice::from_raw_parts(text, outline.end as usize) },
        outline: *outline,
        name: name.as_str(),
        kind: if is_constructor {
            FunctionKind::Normal
        } else {
            FunctionKind::Method
        },
        visibility,
    })
}
