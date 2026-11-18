//! What is in use where a locale is.

use bun_engine_text::StringRef;
use bun_icu::calendar::{Calendar, common_systems};
use bun_icu::resource_bundle::ResourceBundle;
use bun_icu::{Buffer, CText, Enumeration};
use smallvec::SmallVec;

use super::Locale;
use crate::error::IcuResult;
use crate::language_tag::subtag::{Subtag, is_region};
use crate::language_tag::{Key, LanguageTag, join};
use crate::{AvailableLocales, HourCycle, Named, NumberingSystem, Result, calendar, collation};

named_enum! {
    pub enum Direction {
        LeftToRight = "ltr",
        RightToLeft = "rtl",
    }
}

/// Monday is 1 and Sunday is 7.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WeekInfo {
    pub first_day: u8,
    pub weekend: SmallVec<[u8; 7]>,
}

const INVALID_LOCALE: &str = "invalid locale";

impl Locale {
    pub fn calendars(&self) -> Result<Vec<StringRef>> {
        if let Some(preferred) = self.preferred(Key::CALENDAR) {
            return Ok(preferred);
        }
        let names = self.regional_id(&[]).and_then(|id| common_systems(&id));
        collect(names, calendar::bcp47_name)
    }

    pub fn collations(&self) -> Result<Vec<StringRef>> {
        if let Some(preferred) = self.preferred(Key::COLLATION) {
            return Ok(preferred);
        }
        let collations = self
            .data_locale(AvailableLocales::collator())
            .and_then(|id| bun_icu::collator::types_for_locale(&id))
            .and_then(collation::collect)
            .or_type_error(INVALID_LOCALE)?;
        Ok((collations.iter())
            .map(|it| StringRef::from_latin1(it.as_bytes()))
            .collect())
    }

    pub fn hour_cycles(&self) -> Result<Vec<StringRef>> {
        if let Some(preferred) = self.preferred(Key::HOUR_CYCLE) {
            return Ok(preferred);
        }
        let time_data = ResourceBundle::open_direct(c"supplementalData")
            .and_then(|data| data.by_key(c"timeData"))
            .or_type_error(INVALID_LOCALE)?;
        let look_up = |key: &[Subtag]| {
            let entry = time_data.by_key(&join(key.iter().copied()).ok()?).ok()?;
            match u8::try_from(*entry.string_by_key(c"preferred").ok()?.first()?) {
                Ok(b'K') => Some(HourCycle::H11),
                Ok(b'h') => Some(HourCycle::H12),
                Ok(b'H') => Some(HourCycle::H23),
                Ok(b'k') => Some(HourCycle::H24),
                _ => None,
            }
        };
        let language = Subtag::new(self.0.language()).unwrap_or(Subtag::UND);
        let region_override = subdivision_region(&self.0, Key::REGION_OVERRIDE);
        let hour_cycle = (region_override.into_iter())
            .chain(self.preferred_region()?)
            .find_map(|region| look_up(&[language, region]).or_else(|| look_up(&[region])));
        Ok(vec![hour_cycle.unwrap_or(HourCycle::H23).to_string_ref()])
    }

    pub fn numbering_systems(&self) -> Result<Vec<StringRef>> {
        if let Some(preferred) = self.preferred(Key::NUMBERING_SYSTEM) {
            return Ok(preferred);
        }
        let id = (self.data_locale(AvailableLocales::general())).or_type_error(INVALID_LOCALE)?;
        Ok(vec![NumberingSystem::default_for(&id)?.to_string_ref()])
    }

    /// `None` if the locale has no region.
    pub fn time_zones(&self) -> Result<Option<Vec<StringRef>>> {
        let Some(region) = self.base_subtags().region else {
            return Ok(None);
        };
        let names = CText::new(region).and_then(|it| bun_icu::time_zone::canonical_ids(Some(&it)));
        let mut zones = collect(names, |name| name)?;
        zones.sort_unstable_by(|a, b| a.text().cmp(&b.text()));
        Ok(Some(zones))
    }

    /// `direction` of `getTextInfo`
    pub fn direction(&self) -> Result<Direction> {
        let id = self.regional_id(&[]).or_type_error(INVALID_LOCALE)?;
        Ok(match bun_icu::locale::is_right_to_left(&id) {
            true => Direction::RightToLeft,
            false => Direction::LeftToRight,
        })
    }

    pub fn week_info(&self) -> Result<WeekInfo> {
        const UTC: [u16; 3] = [b'U' as u16, b'T' as u16, b'C' as u16];

        let calendar = self
            .regional_id(&[("calendar", Key::CALENDAR), ("fw", Key::FIRST_DAY)])
            .and_then(|id| Calendar::open(&UTC, &id))
            .or_type_error(INVALID_LOCALE)?;
        // ICU counts from Sunday.
        let mut weekend = SmallVec::new();
        for day in 1..=7 {
            if (calendar.starts_in_weekend(i32::from(day) % 7 + 1)).or_type_error(INVALID_LOCALE)? {
                weekend.push(day);
            }
        }
        Ok(WeekInfo {
            first_day: (calendar.first_day_of_week() + 5).rem_euclid(7) as u8 + 1,
            weekend,
        })
    }

    fn preferred(&self, key: Key) -> Option<Vec<StringRef>> {
        let value = self.keyword(key).filter(|it| !it.is_empty())?;
        Some(vec![value])
    }

    /// The root locale if no prefix is available.
    fn data_locale(&self, available: &AvailableLocales) -> bun_icu::Result<CText> {
        CText::new(available.lookup(&self.0).unwrap_or(b"root"))
    }

    /// [[Region]] of RegionPreference
    fn preferred_region(&self) -> Result<Option<Subtag>> {
        let mut id = self.parse()?.language_id;
        id.region = (id.region).or_else(|| subdivision_region(&self.0, Key::SUBDIVISION));
        if id.region.is_none() {
            id.maximize();
        }
        Ok(id.region.or_else(|| Subtag::new(b"001")))
    }

    /// With what determines the region, and those of `keys` whose value is one subtag.
    fn regional_id(&self, keys: &[(&str, Key)]) -> bun_icu::Result<CText> {
        let base = self.base_subtags();
        let mut id = Buffer::from_slice(base.language);
        for subtag in [base.script, base.region].into_iter().flatten() {
            id.push(b'_');
            id.extend_from_slice(subtag);
        }
        let mut separator = b'@';
        let region_keys = [("rg", Key::REGION_OVERRIDE), ("sd", Key::SUBDIVISION)];
        for &(name, key) in keys.iter().chain(&region_keys) {
            if let Some(value) = self.0.keyword(key).and_then(Subtag::new) {
                for part in [&[separator], name.as_bytes(), b"=", value.as_bytes()] {
                    id.extend_from_slice(part);
                }
                separator = b';';
            }
        }
        CText::from_buffer(id)
    }
}

/// CanonicalUnicodeSubdivision
fn subdivision_region(tag: &LanguageTag, key: Key) -> Option<Subtag> {
    const MAX_SUFFIX_LENGTH: usize = 4;

    let subdivision = Subtag::new(tag.keyword(key)?)?;
    let subdivision = subdivision.as_bytes();
    let region_length = if subdivision.first()?.is_ascii_digit() {
        3
    } else {
        2
    };
    let (region, suffix) = subdivision.split_at_checked(region_length)?;
    if !is_region(region) || !(1..=MAX_SUFFIX_LENGTH).contains(&suffix.len()) {
        return None;
    }
    Subtag::new(region).map(Subtag::to_uppercase)
}

fn collect(names: bun_icu::Result<Enumeration>, map: fn(&[u8]) -> &[u8]) -> Result<Vec<StringRef>> {
    let mut names = names.or_type_error(INVALID_LOCALE)?;
    let mut strings = Vec::new();
    while let Some(name) = names.next().or_type_error(INVALID_LOCALE)? {
        strings.push(StringRef::from_latin1(map(name)));
    }
    Ok(strings)
}
