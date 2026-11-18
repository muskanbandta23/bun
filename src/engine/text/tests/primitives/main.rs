//! Each primitive against a plain loop, for every short length and position and for both widths.

#[cfg(not(miri))]
use bun_engine_standalone as _;

mod builder;
mod compare;
mod json;
mod scan;
mod search;
mod support;
mod utf8;
