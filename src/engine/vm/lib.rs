//! The heap, the object model, realms and the run-time operations that every tier calls.

#![feature(
    adt_const_params,
    allocator_ext,
    auto_traits,
    macro_metavar_expr_concat,
    marker_trait_attr,
    maybe_uninit_fill,
    negative_impls,
    thread_local
)]
#![allow(dead_code)]
// References to cells are plain pointers, and a `JsResult<Value>` is one word.
#![allow(improper_ctypes_definitions)]

extern crate self as bun_engine_vm;

pub mod caches;
pub mod call;
mod cell_type;
pub mod code;
mod cx;
mod error;
mod frame;
mod function;
mod hash;
pub mod heap;
mod hooks;
pub mod ic;
mod intrinsic;
mod object;
mod operations;
mod options;
mod primitive;
mod realm;
mod runtime;
pub mod scope;
mod services;
mod traps;
mod value;
mod vm;
mod watchpoint;

pub use call::{CallData, NativeFunction};
pub use cell_type::{CellType, CellTypeRange, TypeFlags};
pub use code::*;
pub use cx::{Cx, JsResult, NoGc, Thrown};
pub use error::*;
pub use frame::{CallFrame, EntryRecord};
pub use function::*;
pub use hash::{FastHashMap, FastHashSet, FastHasher};
pub use heap::{
    CellKind, Extends, Field, FieldArray, Gc, GcCell, GcFree, JsCell, RootVec, Strong, Trace,
    TracedMap, TracedVec, ValueVec, Visitor, Weak, WeakField, root_vec,
};
pub use hooks::{BuiltinSource, Hooks};
pub use intrinsic::{BuiltinId, Intrinsic};
pub use object::*;
pub use operations::*;
pub use options::{Options, options, set_options};
pub use primitive::*;
pub use realm::*;
pub use runtime::*;
pub use scope::{Environment, ModuleEnvironment, ScopeTemplate};
pub use traps::{Trap, Traps};
pub use value::Value;
pub use vm::{DeferTermination, Embedder, Vm, VmRef, VmShapes};
pub use watchpoint::{
    DeferredFire, InlineWatchpointSet, Watcher, Watchpoint, WatchpointSet, WatchpointState,
};
