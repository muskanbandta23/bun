//! Proxies, arguments, wrappers of primitives and enumeration of properties.

mod cloned_arguments;
mod copy;
mod enumerator;
mod literal;
mod mapped_arguments;
mod private;
mod proxy;
mod string_object;
mod wrapper;

pub use cloned_arguments::ClonedArguments;
pub use copy::{clone_object, copy_rest};
pub use enumerator::{EnumerationMode, ForIn, PropertyNameEnumerator};
pub use mapped_arguments::MappedArguments;
pub use proxy::ProxyObject;
pub use string_object::{StringObject, StringObjectInit};
pub use wrapper::{
    BigIntObject, BigIntObjectInit, BooleanObject, BooleanObjectInit, JsWrapperObject,
    JsWrapperObjectInit, NumberObject, NumberObjectInit, SymbolObject, SymbolObjectInit,
};

#[derive(Default)]
pub(crate) struct State {}

impl crate::services::Service for State {}
