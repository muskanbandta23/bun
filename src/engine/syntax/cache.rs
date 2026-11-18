//! What it takes to skip a function that has been parsed before.

use crate::names::{Name, NameKind, Names};
use crate::options::FunctionFlags;
use crate::scope::{Access, Reference, Scopes};
use crate::source::Source;

/// Belongs to one source. It can be dropped at any time.
#[derive(Default)]
pub struct SourceCache {
    pub(crate) functions: Functions,
    /// `Source::bytes` of a source in UTF-16.
    pub(crate) narrowed: Vec<u8>,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) struct Traits: u8 {
        const ARROW = 1 << 0;
        const IN_STRICT_CODE = 1 << 1;
        const CONTAINS_EVAL = 1 << 2;
        const TAGGED_TEMPLATE = 1 << 3;
        const PRIVATE_NAMES = 1 << 4;
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Summary {
    pub(crate) end: u32,
    pub(crate) length: u32,
    pub(crate) flags: FunctionFlags,
    pub(crate) traits: Traits,
}

struct Entry {
    parameters_start: u32,
    summary: Summary,
    /// They end where those of the next entry start.
    first_name: u32,
    /// The index of another entry of the bucket plus one, or zero.
    next: u32,
}

#[derive(Clone, Copy)]
struct FreeName {
    /// Of a constant of [`Name`], its index.
    start: u32,
    /// The length, which is zero for a constant, the kind and the access.
    detail: u32,
}

const LENGTH_SHIFT: u32 = 5;
const KIND_SHIFT: u32 = 3;
/// So many units of the source share a bucket.
const BUCKET_SHIFT: u32 = 7;

#[derive(Default)]
pub(crate) struct Functions {
    buckets: Vec<u32>,
    entries: Vec<Entry>,
    names: Vec<FreeName>,
}

#[derive(Clone, Copy)]
pub(crate) struct Found(usize);

impl Functions {
    #[inline]
    pub(crate) fn find(&self, parameters_start: u32) -> Option<Found> {
        let mut link = *self
            .buckets
            .get((parameters_start >> BUCKET_SHIFT) as usize)?;
        loop {
            let index = (link as usize).wrapping_sub(1);
            let entry = self.entries.get(index)?;
            if entry.parameters_start == parameters_start {
                return Some(Found(index));
            }
            link = entry.next;
        }
    }

    #[inline]
    pub(crate) fn summary(&self, found: Found) -> Summary {
        self.entries[found.0].summary
    }

    /// Makes the references that the function makes to what is around it.
    pub(crate) fn capture_free_names(
        &self,
        found: Found,
        source: Source<'_>,
        names: &mut Names,
        scopes: &mut Scopes,
    ) {
        let end =
            (self.entries.get(found.0 + 1)).map_or(self.names.len(), |it| it.first_name as usize);
        for free in &self.names[self.entries[found.0].first_name as usize..end] {
            let (start, length) = (free.start as usize, (free.detail >> LENGTH_SHIFT) as usize);
            let name = match free.detail >> KIND_SHIFT & 3 {
                _ if length == 0 => Name::from_word(free.start).unwrap_or(Name::EMPTY),
                0 => names.intern_source(source, start, start + length),
                1 => names.intern_narrow(source, start, start + length, NameKind::Private),
                _ => names.intern_narrow(source, start, start + length, NameKind::WellKnownSymbol),
            };
            scopes.capture(name, Access::from_bits_truncate(free.detail as u8));
        }
    }

    /// Does nothing if one of the names is not in the source as it is.
    pub(crate) fn add(
        &mut self,
        parameters_start: u32,
        summary: Summary,
        references: &[Reference],
        source: Source<'_>,
        names: &Names,
    ) {
        let first_name = self.names.len();
        self.names.reserve(references.len());
        for reference in references {
            let (start, length, kind) = match names.place(reference.name) {
                Some(place) if place.1 < u32::MAX >> LENGTH_SHIFT => place,
                _ => return self.names.truncate(first_name),
            };
            let access = u32::from(reference.access.bits());
            self.names.push(FreeName {
                start,
                detail: length << LENGTH_SHIFT | (kind as u32) << KIND_SHIFT | access,
            });
        }
        if self.buckets.is_empty() {
            self.buckets = vec![0; (source.text.len() >> BUCKET_SHIFT) + 1];
        }
        let link = self.entries.len() as u32 + 1;
        let bucket = &mut self.buckets[(parameters_start >> BUCKET_SHIFT) as usize];
        self.entries.push(Entry {
            parameters_start,
            summary,
            first_name: first_name as u32,
            next: core::mem::replace(bucket, link),
        });
    }
}
