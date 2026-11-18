use core::cell::Cell;
use core::ops::Range;

use bun_alloc::AllocError;
use bun_engine_text::{CodeUnit, Text, with_text};
use smallvec::SmallVec;

use crate::{Cx, Gc, ImmutableArray, JsArray, JsResult, JsString, StringView, Value, Vm};

/// The addresses of the `StringImpl`s of two atoms, which cells hold at least until the cache is cleared.
type Key = (usize, usize);

const CACHE_SIZE: usize = 64;
const MAX_CACHED_PIECES: usize = 100_000;
const MAX_ATOMIZED_PIECES: usize = 100;

/// What literals have been split into since the last collection.
pub(super) struct SplitCache([Cell<(Key, usize)>; CACHE_SIZE]);

impl Default for SplitCache {
    fn default() -> SplitCache {
        SplitCache([const { Cell::new(((0, 0), 0)) }; CACHE_SIZE])
    }
}

impl SplitCache {
    #[inline(always)]
    fn entries_for(&self, key: Key) -> [&Cell<(Key, usize)>; 2] {
        let index = (key.0 >> 4 ^ key.0 >> 10) + (key.1 >> 3);
        [index, index + 1].map(|index| &self.0[index % CACHE_SIZE])
    }

    #[inline(always)]
    fn get(&self, key: Key) -> Option<Gc<ImmutableArray>> {
        let entry = self
            .entries_for(key)
            .into_iter()
            .find(|it| it.get().0 == key)?;
        // SAFETY: made since the last collection.
        Some(unsafe { Gc::from_address(entry.get().1) })
    }

    fn insert(&self, key: Key, pieces: Gc<ImmutableArray>) {
        let [first, second] = self.entries_for(key);
        second.set(first.replace((key, pieces.address())));
    }

    pub(super) fn clear(&self) {
        self.0.iter().for_each(|entry| entry.set(((0, 0), 0)));
    }
}

impl StringView {
    /// A substring that is an atom if it looks like a name.
    #[inline]
    pub fn substring_for_key(&self, vm: &Vm, range: Range<usize>) -> Gc<JsString> {
        let text = self.text().slice(range.clone());
        match text.get(0) {
            Some(first) if first.is_ascii_alphabetic() || first.is(b'_') || first.is(b'$') => {
                JsString::from_text_as_atom(vm, text)
            }
            _ => self.substring(vm, range),
        }
    }
}

/// Where each piece ends.
#[inline(always)]
fn find_ends(
    text: Text<'_>,
    separator: Text<'_>,
    limit: usize,
) -> Result<SmallVec<[u32; 64]>, AllocError> {
    let mut ends = SmallVec::new();
    for end in text.matches(separator).chain([text.len()]).take(limit) {
        if ends.len() == ends.capacity() {
            ends.try_reserve(1).map_err(|_| AllocError)?;
        }
        ends.push(end as u32);
    }
    Ok(ends)
}

fn split_view(
    vm: &Vm,
    view: &StringView,
    separator: Text<'_>,
    limit: usize,
    wants_atoms: bool,
) -> Result<Gc<ImmutableArray>, AllocError> {
    if separator.is_empty() {
        return with_text!(view.text(), |units| ImmutableArray::try_from_fn(
            vm,
            units.len().min(limit),
            |index| JsString::from_unit(vm, units[index].to_u16()).into()
        ));
    }
    let ends = find_ends(view.text(), separator, limit)?;
    let wants_atoms = wants_atoms && ends.len() < MAX_ATOMIZED_PIECES;
    let mut start = 0;
    ImmutableArray::try_from_fn(vm, ends.len(), |index| {
        let range = start..ends[index] as usize;
        start = range.end + separator.len();
        Value::from(if wants_atoms {
            view.substring_for_key(vm, range)
        } else {
            view.substring(vm, range)
        })
    })
}

impl JsString {
    /// `String.prototype.split`, for a separator that is a string.
    pub fn split(&self, cx: &Cx, separator: &JsString, limit: u32) -> JsResult<Gc<JsArray>> {
        let cache = &cx.services.text.strings.splits;
        let key = (self.try_impl().zip(separator.try_impl()))
            .filter(|(string, separator)| string.is_atom() && separator.is_atom())
            .map(|(string, separator)| (string.address(), separator.address()))
            .filter(|_| limit == u32::MAX);
        if let Some(pieces) = key.and_then(|key| cache.get(key)) {
            return JsArray::from_immutable(cx, pieces);
        }
        let (view, separator) = (self.view(cx)?, separator.view(cx)?);
        let pieces = split_view(cx, &view, separator.text(), limit as usize, key.is_some());
        let pieces = cx.or_out_of_memory(pieces)?;
        if pieces.is_empty() {
            return Ok(cx.new_array());
        }
        if let Some(key) = key
            && (pieces.len() as usize) < MAX_CACHED_PIECES
        {
            cache.insert(key, pieces);
        }
        JsArray::from_immutable(cx, pieces)
    }
}
