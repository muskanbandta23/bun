//! Source text, what it compiles to, and the state that running code keeps.

mod code_block;
mod compile;
mod executable;
pub mod metadata;
mod source;

pub use code_block::{CodeBlock, CodeOrigin, CompiledCode, InlineFrame, Tier};
pub use compile::{evaluate_script, throw_compile_error};
pub use executable::{
    EvalExecutable, Executable, FunctionExecutable, ModuleExecutable, NativeExecutable,
    ProgramExecutable, ScriptExecutable,
};
pub(crate) use executable::{NativeExecutableKey, native_source_text};
pub use source::{
    LineColumn, SourceCode, SourceProvider, SourceProviderBuilder, SourceProviderRef, SourceType,
};
