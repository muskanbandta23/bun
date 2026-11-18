//! The functions of `js/*.js`. `js::array_prototype::forEach` is the one of that name in `ArrayPrototype.js`.

#![allow(dead_code, non_upper_case_globals, unused_imports)]

use bun_engine_text::{names, private_names, symbols};
use bun_engine_vm::{BuiltinId, BuiltinSource, Intrinsic};

include!(concat!(env!("OUT_DIR"), "/js.rs"));
