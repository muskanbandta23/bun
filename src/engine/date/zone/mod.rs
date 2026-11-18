mod annual;
mod data;
mod id;
mod rules;
mod system;
mod table;

use bun_icu::Buffer;

pub use id::TimeZoneId;
pub use rules::{LocalOffsets, ZoneRules};
pub(crate) use rules::{Offset, Side, Span};
pub use system::{set_time_zone_override, system_time_zone, time_zone_generation};

use crate::AsciiBuffer;

/// A time zone identifier (ECMA-262, 21.4.1.19).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TimeZone {
    Named(TimeZoneId),
    /// Less than a day ahead of UTC or behind it.
    Offset {
        minutes: i16,
    },
}

impl TimeZone {
    /// The name, or FormatOffsetTimeZoneIdentifier.
    pub fn identifier(self) -> Identifier {
        match self {
            TimeZone::Named(id) => Identifier::Name(id.identifier()),
            TimeZone::Offset { minutes } => {
                let mut text = AsciiBuffer::new();
                text.push_offset(minutes, b":");
                Identifier::Offset(text)
            }
        }
    }

    pub fn icu_id(self) -> Buffer<u16> {
        match self {
            TimeZone::Named(id) => id::widen(id.in_icu().identifier()),
            TimeZone::Offset { minutes } => {
                let mut text = AsciiBuffer::<8>::new();
                text.push(b"GMT");
                text.push_offset(minutes, b"");
                id::widen(&text)
            }
        }
    }

    /// TimeZoneEquals.
    pub fn is_same_zone(self, other: TimeZone) -> bool {
        match (self, other) {
            (TimeZone::Named(one), TimeZone::Named(other)) => {
                one == other || one.primary() == other.primary()
            }
            _ => self == other,
        }
    }
}

pub enum Identifier {
    Name(&'static str),
    Offset(AsciiBuffer<6>),
}

impl core::ops::Deref for Identifier {
    type Target = str;

    fn deref(&self) -> &str {
        match self {
            Identifier::Name(name) => name,
            Identifier::Offset(text) => text,
        }
    }
}
