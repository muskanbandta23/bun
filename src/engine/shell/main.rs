//! Runs scripts and modules on the engine alone, for conformance suites and benchmarks.

#![feature(negative_impls)]
#![allow(improper_ctypes_definitions)]
// The wrappers that these lints point to need the rest of Bun.
#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::disallowed_macros
)]

mod command_line;
mod files;
mod functions;
mod global;
mod inspector;
mod modules;
mod run;
mod state;

use bun_engine_standalone as _;

use crate::command_line::CommandLine;

/// What nearly every module of this crate uses.
mod prelude {
    pub(crate) use bun_engine_macros::native_abi;
    pub(crate) use bun_engine_text::{Identifier, StringRef};
    pub(crate) use bun_engine_vm::{
        Attributes, CallFrame, Cx, ErrorType, Gc, JsObject, JsResult, JsString, NativeFunction,
        Value,
    };

    pub(crate) use crate::functions::Functions;
    pub(crate) use crate::state::State;
}

fn main() {
    let (command_line, options) = CommandLine::parse(std::env::args_os().skip(1));
    bun_engine_vm::set_options(options);
    let code = run::run(&command_line, |cx| run::run_scripts(cx, &command_line));
    if command_line.prints_footprint {
        let (current, peak) = functions::footprint();
        println!("Memory Footprint:\n    Current Footprint: {current}\n    Peak Footprint: {peak}");
    }
    run::exit(code)
}
