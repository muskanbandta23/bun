//! `Intl.NumberFormat`

mod digit_options;
pub(crate) mod parts;
mod skeleton;

pub use digit_options::{
    DigitOptions, DigitRange, MinimumIntegerDigits, RequestedDigits, RoundingIncrement,
    RoundingMode, RoundingPriority, RoundingType, TrailingZeroDisplay,
};
pub(crate) use skeleton::Skeleton;

option_enum! {
    #[derive(Default)]
    pub enum Notation: "notation must be either \"standard\", \"scientific\", \"engineering\", or \"compact\"" {
        #[default]
        Standard = "standard",
        Scientific = "scientific",
        Engineering = "engineering",
        Compact = "compact",
    }
}

option_enum! {
    #[derive(Default)]
    pub enum CompactDisplay: "compactDisplay must be either \"short\" or \"long\"" {
        #[default]
        Short = "short",
        Long = "long",
    }
}
