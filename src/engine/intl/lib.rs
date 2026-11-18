//! The `Intl` object.

#![forbid(unsafe_code)]
#![expect(
    dead_code,
    unused_imports,
    reason = "the constructors that use it are to come"
)]

#[macro_use]
mod options;
mod available_locales;
mod cache;
mod calendar;
mod collation;
pub mod date_time_format;
mod error;
mod language_tag;
mod locale_list;
mod locale_resolver;
pub mod number_format;
mod numbering_system;
mod parts;
mod recent;

pub use available_locales::AvailableLocales;
pub use cache::Cache;
pub use error::{Error, ErrorKind, Result};
pub use language_tag::{KeywordValue, LanguageTag};
pub use locale_list::LocaleList;
pub use numbering_system::NumberingSystem;
pub use options::{HourCycle, LocaleMatcher, Named, OptionValue, integer_option};
pub use parts::{Part, PartKind, Parts, Source, TimeUnit};
