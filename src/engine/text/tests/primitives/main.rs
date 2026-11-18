//! Each primitive against a plain loop, for every short length and position and for both widths.

#[cfg(not(miri))]
use bun_engine_standalone as _;

mod binary;
mod builder;
mod compare;
mod json;
mod replace;
mod scan;
mod search;
mod support;
mod uri;
mod utf8;
