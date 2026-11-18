//! UAX #15.

use smallvec::SmallVec;

use crate::error::Status;
use crate::ffi::{call, fill, length, open};
use crate::{Buffer, Error, Result};

bun_opaque::opaque_ffi! { struct UNormalizer2; }

icu_functions! {
    fn unorm2_getNFCInstance(status: &mut Status) -> *mut UNormalizer2;
    fn unorm2_getNFDInstance(status: &mut Status) -> *mut UNormalizer2;
    fn unorm2_getNFKCInstance(status: &mut Status) -> *mut UNormalizer2;
    fn unorm2_getNFKDInstance(status: &mut Status) -> *mut UNormalizer2;
    fn unorm2_spanQuickCheckYes(normalizer: *const UNormalizer2, text: *const u16, length: i32, status: &mut Status) -> i32;
    fn unorm2_hasBoundaryBefore(normalizer: *const UNormalizer2, code_point: u32) -> i8;
    fn unorm2_normalize(
        normalizer: *const UNormalizer2,
        text: *const u16,
        length: i32,
        out: *mut u16,
        capacity: i32,
        status: &mut Status,
    ) -> i32;
    fn u_getCombiningClass(code_point: u32) -> u8;
}

/// A piece ends at the first boundary after so many units.
const PIECE_LENGTH: usize = 512;
const MAX_PIECE_LENGTH: usize = 1 << 28;

type Scratch = SmallVec<[u16; 128]>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}

impl Form {
    fn normalizer(self) -> Result<Normalizer> {
        let instance = match self {
            Form::Nfc => unorm2_getNFCInstance,
            Form::Nfd => unorm2_getNFDInstance,
            Form::Nfkc => unorm2_getNFKCInstance,
            Form::Nfkd => unorm2_getNFKDInstance,
        };
        // SAFETY: it takes nothing but the status.
        open(|status| unsafe { instance(status) }).map(|it| Normalizer(it.as_ptr()))
    }

    fn decomposing(self) -> Form {
        match self {
            Form::Nfc | Form::Nfd => Form::Nfd,
            Form::Nfkc | Form::Nfkd => Form::Nfkd,
        }
    }

    pub fn is_normalized(self, text: &[u16]) -> Result<bool> {
        let mut is_normalized = true;
        let rest = &text[self.normalized_prefix(text)?..];
        self.normalize_pieces(rest, |piece, normalized| {
            is_normalized &= piece == normalized;
        })?;
        Ok(is_normalized)
    }

    /// The length of a prefix that is normalized, whatever follows it. It need not be the longest.
    pub fn normalized_prefix(self, text: &[u16]) -> Result<usize> {
        let (normalizer, length) = (self.normalizer()?, length(text)?);
        // SAFETY: `text` has `length` units.
        call(|status| unsafe {
            unorm2_spanQuickCheckYes(normalizer.0, text.as_ptr(), length, status)
        })
        .map(|span| usize::try_from(span).unwrap_or(0).min(text.len()))
    }

    /// Calls `take` with one piece of `text` after another, and what it normalizes to.
    pub fn normalize_pieces(
        self,
        mut text: &[u16],
        mut take: impl FnMut(&[u16], &[u16]),
    ) -> Result<()> {
        let normalizer = self.normalizer()?;
        let mut normalized = Scratch::new();
        while !text.is_empty() {
            let piece;
            (piece, text) = text.split_at(normalizer.piece_length(text));
            if piece.len() <= 2 * PIECE_LENGTH {
                normalizer.normalize(piece, 3 * piece.len(), &mut normalized)?;
                take(piece, &normalized);
                continue;
            }
            let decomposed = self.decomposing().normalizer()?.decompose_long(piece)?;
            if self == self.decomposing() {
                take(piece, &decomposed);
            } else {
                normalizer.normalize(&decomposed, decomposed.len(), &mut normalized)?;
                take(piece, &normalized);
            }
        }
        Ok(())
    }

    pub fn normalize(self, text: &[u16], out: &mut Buffer<u16>) -> Result<()> {
        out.clear();
        let mut has_room = true;
        self.normalize_pieces(text, |_, normalized| {
            has_room = has_room && out.try_reserve(normalized.len()).is_ok();
            if has_room {
                out.extend_from_slice(normalized);
            }
        })?;
        has_room.then_some(()).ok_or(Error::MEMORY_ALLOCATION)
    }
}

/// A constant of the process that ICU owns.
#[derive(Clone, Copy)]
struct Normalizer(*const UNormalizer2);

impl Normalizer {
    fn piece_length(self, text: &[u16]) -> usize {
        let mut length = code_point_boundary(text, PIECE_LENGTH);
        for code_point in code_points(&text[length..]) {
            // SAFETY: it takes any code point.
            if unsafe { unorm2_hasBoundaryBefore(self.0, code_point) } != 0 {
                break;
            }
            length += unit_count(code_point);
        }
        length
    }

    fn normalize(self, text: &[u16], expected_length: usize, out: &mut Scratch) -> Result<()> {
        let length = length(text)?;
        out.clear();
        (out.try_reserve(expected_length)).map_err(|_| Error::MEMORY_ALLOCATION)?;
        // SAFETY: `text` has `length` units, and `out` has room for `capacity`.
        fill(out, |out, capacity, status| unsafe {
            unorm2_normalize(self.0, text.as_ptr(), length, out, capacity, status)
        })
    }

    /// For text in which any number of combining marks follow each other.
    fn decompose_long(self, mut text: &[u16]) -> Result<Vec<u16>> {
        const CHUNK_LENGTH: usize = 32;

        let mut decomposed = Vec::new();
        let mut scratch = Scratch::new();
        while !text.is_empty() {
            let chunk;
            (chunk, text) = text.split_at(code_point_boundary(text, CHUNK_LENGTH));
            self.normalize(chunk, 0, &mut scratch)?;
            if decomposed.len() > MAX_PIECE_LENGTH || decomposed.try_reserve(scratch.len()).is_err()
            {
                return Err(Error::MEMORY_ALLOCATION);
            }
            decomposed.extend_from_slice(&scratch);
        }
        order_marks(&mut decomposed)?;
        Ok(decomposed)
    }
}

/// The Canonical Ordering Algorithm.
fn order_marks(mut text: &mut [u16]) -> Result<()> {
    const CLASS_SHIFT: u32 = 56;
    const INDEX_SHIFT: u32 = 24;

    let mut marks = Vec::<u64>::new();
    while !text.is_empty() {
        let mut starter = None;
        for code_point in code_points(text) {
            // SAFETY: it takes any code point.
            let class = unsafe { u_getCombiningClass(code_point) };
            if class == 0 {
                starter = Some(code_point);
                break;
            }
            marks.try_reserve(1).map_err(|_| Error::MEMORY_ALLOCATION)?;
            let index = marks.len() as u64;
            marks.push(
                u64::from(class) << CLASS_SHIFT | index << INDEX_SHIFT | u64::from(code_point),
            );
        }
        marks.sort_unstable();
        let mut length = 0;
        for mark in marks.drain(..) {
            let mark = char::from_u32(mark as u32 & ((1 << INDEX_SHIFT) - 1)).unwrap_or_default();
            length += mark.encode_utf16(&mut text[length..]).len();
        }
        text = &mut text[length + starter.map_or(0, unit_count)..];
    }
    Ok(())
}

/// A lone surrogate is itself.
fn code_points(text: &[u16]) -> impl Iterator<Item = u32> {
    char::decode_utf16(text.iter().copied())
        .map(|it| it.map_or_else(|lone| lone.unpaired_surrogate().into(), u32::from))
}

fn unit_count(code_point: u32) -> usize {
    1 + usize::from(code_point > 0xffff)
}

/// The first at or after `index`.
fn code_point_boundary(text: &[u16], index: usize) -> usize {
    match text.get(index - 1..) {
        Some([0xd800..=0xdbff, 0xdc00..=0xdfff, ..]) => index + 1,
        Some(_) => index.min(text.len()),
        None => text.len(),
    }
}
