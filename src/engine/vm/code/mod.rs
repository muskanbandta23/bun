//! Source text, what it compiles to, and the state that running code keeps.

mod builtin;
mod builtin_values;
mod cache;
mod code_block;
mod compile;
mod discard;
mod dynamic_function;
mod eval;
mod executable;
pub mod metadata;
mod optimization;
mod profile;
mod script;
mod source;
mod speculation;

pub use builtin::{BuiltinOutline, describe_builtin};
pub use code_block::{CodeBlock, CodeOrigin, CompiledCode, CoverageCount, InlineFrame, Tier};
pub use compile::{
    CompileError, CompileErrorKind, CompileErrorRecovery, create_compile_error,
    create_compile_error_in, throw_compile_error,
};
pub use dynamic_function::{DynamicFunctionKind, create_dynamic_function};
pub use eval::{
    direct_eval, execute_indirect_eval, indirect_eval, origin_of_caller, prepare_indirect_eval,
};
pub(crate) use eval::evaluate_in_scope;
pub use executable::{
    EvalExecutable, Executable, FunctionExecutable, ModuleExecutable, NativeExecutable,
    ProgramExecutable, ScriptExecutable, native_source_text,
};
pub(crate) use executable::NativeExecutableKey;
pub use optimization::{ExitKind, ExitKinds, OptimizerState};
pub use profile::{Arithmetic, ArithmeticProfile, ArrayModes, ArrayProfile, ValueProfile};
pub use script::{evaluate_compiled_script, evaluate_script, evaluate_script_in_scope};
pub use source::{
    LineColumn, SourceCode, SourceProvider, SourceProviderBuilder, SourceProviderRef, SourceType,
};
pub use speculation::SpeculatedType;

use core::cell::RefCell;

use crate::Vm;
use crate::heap::Scope;

#[derive(Default)]
pub(crate) struct State {
    compilation: compile::Compilation,
    builtins: builtin::Builtins,
    last_function: RefCell<Option<dynamic_function::LastFunction>>,
}

impl crate::services::Service for State {
    fn did_collect(&self, vm: &Vm, scope: Scope) {
        if scope == Scope::Full {
            vm.prune_compilation_caches();
        }
    }
}
