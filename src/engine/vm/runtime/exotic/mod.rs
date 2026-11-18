//! Proxies, arguments, wrappers of primitives and enumeration of properties.

mod enumerator;
mod proxy;
mod string_object;
mod wrapper;

pub use enumerator::PropertyNameEnumerator;
pub use proxy::ProxyObject;
pub use string_object::{StringObject, StringObjectInit};
pub use wrapper::{
    BigIntObject, BigIntObjectInit, BooleanObject, BooleanObjectInit, JsWrapperObject,
    JsWrapperObjectInit, NumberObject, NumberObjectInit, SymbolObject, SymbolObjectInit,
};

#[derive(Default)]
pub(crate) struct State {}

impl crate::services::Service for State {}
