//! Fails to compile where `include/engine/api/Layout.h` is wrong.

#![allow(non_upper_case_globals, clippy::unnecessary_cast)]

use bun_engine_text::*;
use bun_engine_vm::heap::*;
use bun_engine_vm::*;

/// A mismatch names both numbers.
struct Is<const N: usize>;

include!(concat!(env!("OUT_DIR"), "/layout.rs"));
