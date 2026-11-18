//! Safe bindings to the parts of ICU that Bun calls. A module for each service declares the functions it wraps.

#[macro_use]
mod ffi;
mod c_text;
pub mod calendar;
pub mod date_pattern;
mod enumeration;
mod error;
mod host_language;
pub mod locale;
pub mod normalizer;
pub mod resource_bundle;
pub mod time_zone;
mod version;

pub use c_text::CText;
pub use enumeration::Enumeration;
pub use error::{Error, Result};
pub use ffi::Buffer;
pub use host_language::host_language;
pub use version::{Version, library_version, unicode_version};
