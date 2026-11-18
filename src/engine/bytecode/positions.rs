//! Which part of the source an instruction stands for.

/// An error is reported at `divot` and quotes the text from `start` to `end`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Position {
    pub divot: u32,
    pub start: u32,
    pub end: u32,
}

const ENTRIES_PER_CHAPTER: usize = 64;

/// Where decoding can start.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Chapter {
    offset: u32,
    divot: u32,
    at: u32,
}

/// Each entry is four numbers of seven bits a byte: how far on its instruction and its divot are from those of the
/// entry before, and how far the range extends to either side.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Positions {
    bytes: Box<[u8]>,
    chapters: Box<[Chapter]>,
}

fn write(bytes: &mut Vec<u8>, mut value: u32) {
    while value >= 0x80 {
        bytes.push(value as u8 | 0x80);
        value >>= 7;
    }
    bytes.push(value as u8);
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

impl Positions {
    /// The offsets ascend.
    pub(crate) fn encode(
        entries: impl Iterator<Item = (u32, Position)>,
        scratch: &mut Vec<u8>,
    ) -> Positions {
        scratch.clear();
        let mut chapters = Vec::new();
        let (mut last_offset, mut last_divot) = (0, 0u32);
        for (index, (offset, position)) in entries.enumerate() {
            if index != 0 && index % ENTRIES_PER_CHAPTER == 0 {
                chapters.push(Chapter {
                    offset: last_offset,
                    divot: last_divot,
                    at: scratch.len() as u32,
                });
            }
            let step = position.divot.wrapping_sub(last_divot) as i32;
            write(scratch, offset - last_offset);
            write(scratch, (step << 1 ^ step >> 31) as u32);
            write(scratch, position.divot.wrapping_sub(position.start));
            write(scratch, position.end.wrapping_sub(position.divot));
            (last_offset, last_divot) = (offset, position.divot);
        }
        Positions {
            bytes: scratch.as_slice().into(),
            chapters: chapters.into(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    fn entries_from(&self, chapter: Chapter) -> impl Iterator<Item = (u32, Position)> + '_ {
        let mut bytes = self.bytes.get(chapter.at as usize..).unwrap_or_default();
        let (mut offset, mut divot) = (chapter.offset, chapter.divot);
        core::iter::from_fn(move || {
            offset = offset.wrapping_add(read(&mut bytes)?);
            let step = read(&mut bytes)?;
            divot = divot.wrapping_add(step >> 1 ^ (step & 1).wrapping_neg());
            let position = Position {
                divot,
                start: divot.wrapping_sub(read(&mut bytes)?),
                end: divot.wrapping_add(read(&mut bytes)?),
            };
            Some((offset, position))
        })
    }

    /// Each holds from its offset up to that of the next.
    pub fn entries(&self) -> impl Iterator<Item = (u32, Position)> + '_ {
        self.entries_from(Chapter {
            offset: 0,
            divot: 0,
            at: 0,
        })
    }

    /// None before the first entry, where the start of the code has to do.
    pub fn find(&self, offset: u32) -> Option<Position> {
        let last = |entries: &mut dyn Iterator<Item = (u32, Position)>| {
            let entries = entries.take_while(|&(start, _)| start <= offset);
            entries.last().map(|(_, position)| position)
        };
        match self
            .chapters
            .partition_point(|chapter| chapter.offset <= offset)
        {
            0 | 1 => last(&mut self.entries()),
            after => last(&mut self.entries_from(self.chapters[after - 2])),
        }
    }
}
