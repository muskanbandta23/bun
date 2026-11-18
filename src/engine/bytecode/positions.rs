//! Which part of the source an instruction stands for.

/// An error is reported at `divot` and quotes the text from `start` to `end`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Position {
    pub divot: u32,
    pub start: u32,
    pub end: u32,
}

const ENTRIES_PER_CHAPTER: usize = 64;
const CHAPTER_BYTES: usize = 12;
const MAX_NUMBER_BYTES: usize = 5;

/// Where decoding can start.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Chapter {
    /// Of its first entry.
    offset: u32,
    /// Of the entry before that.
    divot: u32,
    at: u32,
}

impl Chapter {
    fn read(bytes: &[u8; CHAPTER_BYTES]) -> Chapter {
        let word = |index: usize| u32::from_le_bytes(bytes.as_chunks().0[index]);
        Chapter {
            offset: word(0),
            divot: word(1),
            at: word(2),
        }
    }

    fn write(self, bytes: &mut [u8]) {
        let words = [self.offset, self.divot, self.at].map(u32::to_le_bytes);
        bytes[..CHAPTER_BYTES].copy_from_slice(words.as_flattened());
    }
}

/// Each entry is four numbers of seven bits a byte: how far on its instruction and its divot are from those of the
/// entry before, and how far the range extends to either side.
#[derive(Clone, Copy, Default)]
pub struct Positions<'a> {
    chapters: &'a [[u8; CHAPTER_BYTES]],
    entries: &'a [u8],
}

#[inline(always)]
fn write(bytes: &mut [u8], mut at: usize, mut value: u32) -> usize {
    while value >= 0x80 {
        bytes[at] = value as u8 | 0x80;
        value >>= 7;
        at += 1;
    }
    bytes[at] = value as u8;
    at + 1
}

fn read(bytes: &mut &[u8]) -> Option<u32> {
    let mut value = 0u32;
    for shift in (0..35).step_by(7) {
        let (&byte, rest) = bytes.split_first()?;
        *bytes = rest;
        value |= u32::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Some(value);
        }
    }
    None
}

struct Entries<'a> {
    bytes: &'a [u8],
    offset: u32,
    divot: u32,
}

impl Iterator for Entries<'_> {
    type Item = (u32, Position);

    fn next(&mut self) -> Option<(u32, Position)> {
        let mut bytes = self.bytes;
        let offset = self.offset.wrapping_add(read(&mut bytes)?);
        let step = read(&mut bytes)?;
        let divot = self
            .divot
            .wrapping_add(step >> 1 ^ (step & 1).wrapping_neg());
        let position = Position {
            divot,
            start: divot.wrapping_sub(read(&mut bytes)?),
            end: divot.wrapping_add(read(&mut bytes)?),
        };
        *self = Entries {
            bytes,
            offset,
            divot,
        };
        Some((offset, position))
    }
}

impl<'a> Positions<'a> {
    /// The most bytes that so many entries take.
    pub(crate) fn max_size(entries: usize) -> usize {
        MAX_NUMBER_BYTES * (1 + 4 * entries) + entries / ENTRIES_PER_CHAPTER * CHAPTER_BYTES
    }

    /// Returns how much of `bytes` it took: the number of chapters, the chapters, the entries. The offsets ascend.
    pub(crate) fn encode(
        entries: impl ExactSizeIterator<Item = (u32, Position)>,
        bytes: &mut [u8],
    ) -> usize {
        let Some(last) = entries.len().checked_sub(1) else {
            return 0;
        };
        let chapters = last / ENTRIES_PER_CHAPTER;
        let mut chapter_at = write(bytes, 0, chapters as u32);
        let first = chapter_at + chapters * CHAPTER_BYTES;
        let mut at = first;
        let (mut last_offset, mut last_divot) = (0, 0u32);
        for (index, (offset, position)) in entries.enumerate() {
            if index != 0 && index % ENTRIES_PER_CHAPTER == 0 {
                let chapter = Chapter {
                    offset,
                    divot: last_divot,
                    at: (at - first) as u32,
                };
                chapter.write(&mut bytes[chapter_at..]);
                chapter_at += CHAPTER_BYTES;
            }
            let step = position.divot.wrapping_sub(last_divot) as i32;
            at = write(bytes, at, offset - last_offset);
            at = write(bytes, at, (step << 1 ^ step >> 31) as u32);
            at = write(bytes, at, position.divot.wrapping_sub(position.start));
            at = write(bytes, at, position.end.wrapping_sub(position.divot));
            (last_offset, last_divot) = (offset, position.divot);
        }
        at
    }

    pub(crate) fn read(mut bytes: &'a [u8]) -> Option<Positions<'a>> {
        if bytes.is_empty() {
            return Some(Positions::default());
        }
        let size = (read(&mut bytes)? as usize).checked_mul(CHAPTER_BYTES)?;
        let (chapters, entries) = bytes.split_at_checked(size)?;
        Some(Positions {
            chapters: chapters.as_chunks().0,
            entries,
        })
    }

    pub fn is_empty(self) -> bool {
        self.entries.is_empty()
    }

    fn all(self) -> Entries<'a> {
        Entries {
            bytes: self.entries,
            offset: 0,
            divot: 0,
        }
    }

    fn entries_from(self, chapter: Chapter) -> Entries<'a> {
        let bytes = self.entries.get(chapter.at as usize..).unwrap_or_default();
        let step = read(&mut { bytes }).unwrap_or(0);
        Entries {
            bytes,
            offset: chapter.offset.wrapping_sub(step),
            divot: chapter.divot,
        }
    }

    /// Each holds from its offset up to that of the next.
    pub fn entries(self) -> impl Iterator<Item = (u32, Position)> + 'a {
        self.all()
    }

    /// The first entry holds for what is before it too. If there is none, the start of the code has to do.
    pub fn find(self, offset: u32) -> Option<Position> {
        let chapters_begun = self
            .chapters
            .partition_point(|chapter| Chapter::read(chapter).offset <= offset);
        let entries = match self.chapters[..chapters_begun].last() {
            Some(chapter) => self.entries_from(Chapter::read(chapter)),
            None => self.all(),
        };
        let entries = entries.take_while(|&(start, _)| start <= offset);
        let found = entries.last().or_else(|| self.all().next());
        found.map(|(_, position)| position)
    }

    /// Whether all bytes decode, the offsets ascend and are accepted, and the chapters agree with the entries.
    pub(crate) fn is_valid(self, accepts: impl Fn(u32) -> bool) -> bool {
        let mut entries = self.all();
        let mut chapters = self.chapters.iter();
        for index in 0usize.. {
            let (last_offset, divot) = (entries.offset, entries.divot);
            let at = (self.entries.len() - entries.bytes.len()) as u32;
            let Some((offset, _)) = entries.next() else {
                break;
            };
            if !accepts(offset) || (index != 0 && offset <= last_offset) {
                return false;
            }
            if index != 0
                && index % ENTRIES_PER_CHAPTER == 0
                && chapters.next().map(Chapter::read) != Some(Chapter { offset, divot, at })
            {
                return false;
            }
        }
        entries.bytes.is_empty() && chapters.next().is_none()
    }
}
