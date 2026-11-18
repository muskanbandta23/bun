//! Procedural macros of the engine. What they expand to is documented beside the traits of `bun_engine_vm`.

mod cell;
mod native_abi;
mod object_ops;
mod trace;

use proc_macro::TokenStream;

fn expand(result: syn::Result<proc_macro2::TokenStream>) -> TokenStream {
    result.unwrap_or_else(syn::Error::into_compile_error).into()
}

/// Declares a type of cell. See `bun_engine_vm::CellKind`.
#[proc_macro_attribute]
pub fn cell(arguments: TokenStream, item: TokenStream) -> TokenStream {
    expand(cell::expand(arguments.into(), item.into()))
}

/// The internal methods that a type of object overrides. See `bun_engine_vm::ClassOps`.
#[proc_macro_attribute]
pub fn object_ops(_: TokenStream, item: TokenStream) -> TokenStream {
    expand(object_ops::expand(item.into()))
}

/// Traces every field that can refer to a cell.
#[proc_macro_derive(Trace)]
pub fn derive_trace(item: TokenStream) -> TokenStream {
    expand(trace::expand(item.into()))
}

/// Gives a function, or an alias of a function pointer, the calling convention that generated code calls with.
#[proc_macro_attribute]
pub fn native_abi(_: TokenStream, item: TokenStream) -> TokenStream {
    expand(native_abi::expand(item.into()))
}
