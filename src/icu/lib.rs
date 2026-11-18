//! Safe bindings to the parts of ICU that Bun calls. A module for each service declares the functions it wraps.

#[macro_use]
mod ffi;
mod c_text;
mod enumeration;
mod error;
pub mod locale;
pub mod normalizer;
mod version;

pub use c_text::CText;
pub use enumeration::Enumeration;
pub use error::{Error, Result};
pub use ffi::Buffer;
pub use version::{Version, library_version, unicode_version};
