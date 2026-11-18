//! Function objects.

mod bound_function;
mod custom_accessor_function;
mod internal_function;
mod js_function;
mod rare_data;

pub use bound_function::JsBoundFunction;
pub(crate) use custom_accessor_function::CustomAccessorKey;
pub use custom_accessor_function::{JsCustomGetterFunction, JsCustomSetterFunction};
pub use internal_function::{InternalFunction, InternalFunctionInit, function_realm};
pub use js_function::{JsCallee, JsFunction, JsFunctionInit, display_name};
pub use rare_data::FunctionRareData;
