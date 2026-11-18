use core::ffi::CStr;
use std::sync::OnceLock;

use bun_icu::Buffer;

use crate::LanguageTag;
use crate::language_tag::subtag::{Subtags, is_region, is_script, is_variant};

/// A tag as a number. No locale that has data needs more bytes.
fn pack(tag: &[u8]) -> Option<u128> {
    let mut padded = [0; size_of::<u128>()];
    padded.get_mut(..tag.len())?.copy_from_slice(tag);
    Some(u128::from_be_bytes(padded))
}

fn pack_subtags<'a>(subtags: impl Iterator<Item = &'a [u8]>) -> Option<u128> {
    let mut tag = Buffer::new();
    for subtag in subtags.filter(|it| !it.is_empty()) {
        if !tag.is_empty() {
            tag.push(b'-');
        }
        let start = tag.len();
        tag.extend_from_slice(subtag);
        if start != 0 && !is_script(subtag) && is_variant(subtag) {
            tag[start..].make_ascii_lowercase();
        }
    }
    pack(&tag)
}

/// The [[AvailableLocales]] of a constructor.
pub struct AvailableLocales {
    sorted: Box<[u128]>,
}

impl AvailableLocales {
    pub fn collator() -> &'static AvailableLocales {
        static LOCALES: OnceLock<AvailableLocales> = OnceLock::new();
        LOCALES.get_or_init(|| AvailableLocales::new(bun_icu::collator::available_locales()))
    }

    /// Of the other constructors.
    pub fn general() -> &'static AvailableLocales {
        static LOCALES: OnceLock<AvailableLocales> = OnceLock::new();
        LOCALES.get_or_init(|| AvailableLocales::new(bun_icu::locale::available()))
    }

    fn new(ids: impl Iterator<Item = &'static CStr>) -> AvailableLocales {
        let mut tags = Vec::with_capacity(ids.size_hint().1.unwrap_or(0));
        for id in ids {
            let subtags = Subtags::new(id.to_bytes());
            tags.extend(pack_subtags(subtags.clone()));
            // An available locale with a script and a region is also available without the script.
            let mut rest = subtags;
            if let (Some(language), Some(_), Some(region), None) = (
                rest.next(),
                rest.next_if(is_script),
                rest.next_if(is_region),
                rest.next(),
            ) {
                tags.extend(pack_subtags([language, region].into_iter()));
            }
        }
        tags.sort_unstable();
        tags.dedup();
        AvailableLocales {
            sorted: tags.into(),
        }
    }

    fn contains(&self, tag: &[u8]) -> bool {
        pack(tag).is_some_and(|tag| self.sorted.binary_search(&tag).is_ok())
    }

    /// The longest prefix of `tag` that is available.
    pub(crate) fn lookup<'a>(&self, tag: &'a LanguageTag) -> Option<&'a [u8]> {
        let base_name = tag.base_name();
        let mut subtags = Subtags::new(base_name);
        let mut length = 0;
        let mut found = None;
        while let Some(subtag) = subtags.next().filter(|_| length < size_of::<u128>()) {
            length += usize::from(length != 0) + subtag.len();
            if self.contains(&base_name[..length]) {
                found = Some(&base_name[..length]);
            }
        }
        found
    }

    /// FilterLocales
    pub fn filter<'a>(
        &'a self,
        requested: &'a [LanguageTag],
    ) -> impl Iterator<Item = &'a LanguageTag> {
        requested.iter().filter(|tag| self.lookup(tag).is_some())
    }
}
