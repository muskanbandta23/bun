use bun_engine_text::Text;

use crate::{Error, InputError, Result, TextError, parse};

macro_rules! calendars {
    ($($name:ident = $identifier:literal,)*) => {
        /// An available calendar, by its canonical identifier.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        #[repr(u8)]
        pub enum CalendarId {
            $($name,)*
        }

        impl CalendarId {
            /// AvailableCalendars, in the order of their identifiers.
            pub const ALL: &[CalendarId] = &[$(CalendarId::$name,)*];

            pub const fn identifier(self) -> &'static str {
                match self {
                    $(CalendarId::$name => $identifier,)*
                }
            }
        }
    };
}

calendars! {
    Buddhist = "buddhist",
    Chinese = "chinese",
    Coptic = "coptic",
    Dangi = "dangi",
    Ethioaa = "ethioaa",
    Ethiopic = "ethiopic",
    Gregory = "gregory",
    Hebrew = "hebrew",
    Indian = "indian",
    IslamicCivil = "islamic-civil",
    IslamicTbla = "islamic-tbla",
    IslamicUmalqura = "islamic-umalqura",
    Iso8601 = "iso8601",
    Japanese = "japanese",
    Persian = "persian",
    Roc = "roc",
}

const ALIASES: [(&str, CalendarId); 2] = [
    ("ethiopic-amete-alem", CalendarId::Ethioaa),
    ("islamicc", CalendarId::IslamicCivil),
];

impl CalendarId {
    /// By its identifier or an alias, ignoring ASCII case.
    pub fn find(name: Text<'_>) -> Option<CalendarId> {
        (CalendarId::ALL.iter())
            .map(|&calendar| (calendar.identifier(), calendar))
            .chain(ALIASES)
            .find(|(identifier, _)| name.eq_ignore_ascii_case(Text::from_ascii(identifier)))
            .map(|(_, calendar)| calendar)
    }

    /// CanonicalizeCalendar
    pub fn canonicalize(name: Text<'_>) -> Result<CalendarId, InputError<'_>> {
        CalendarId::find(name).ok_or(InputError::Text(TextError::Calendar, name))
    }

    /// CanonicalizeCalendar, in the constructors of the plain types.
    pub fn argument(name: Text<'_>) -> Result<CalendarId> {
        CalendarId::find(name).ok_or(Error::InvalidCalendarArgument)
    }

    /// ToTemporalCalendarIdentifier of a string.
    pub fn parse(text: Text<'_>) -> Result<CalendarId, InputError<'_>> {
        if let Some(calendar) = CalendarId::find(text) {
            return Ok(calendar);
        }
        match parse::any_date_or_time(text) {
            Some((_, annotations)) if annotations.can_name_calendar() => {
                CalendarId::from_annotation(annotations.calendar)
            }
            _ => Err(InputError::Text(TextError::Calendar, text)),
        }
    }

    #[inline]
    pub(crate) fn from_annotation(name: Option<Text<'_>>) -> Result<CalendarId, InputError<'_>> {
        name.map_or(Ok(CalendarId::Iso8601), CalendarId::canonicalize)
    }

    pub(super) fn is_chinese_or_dangi(self) -> bool {
        matches!(self, CalendarId::Chinese | CalendarId::Dangi)
    }
}
