//! Where the lines of a source start.

use core::ops::Range;

use bun_engine_text::Text;

use crate::source::narrow;

/// Lines that start within `u16::MAX` units of the first.
struct Block {
    start: u32,
    first_line: u32,
}

/// Lines and columns count from zero.
pub struct LineTable {
    blocks: Box<[Block]>,
    /// Of each line, from the start of its block.
    offsets: Box<[u16]>,
}

fn line_starts(text: Text<'_>, bytes: &[u8]) -> Vec<u32> {
    if bun_highway::index_of_char(bytes, b'\r').is_none() {
        return bun_highway::line_starts(bytes);
    }
    let mut starts = vec![0];
    let mut offset = 0;
    while let Some(length) = bun_highway::index_of_any_char(&bytes[offset..], b"\n\r") {
        offset += length + 1;
        if bytes[offset - 1] == b'\r' && text.get(offset) == Some(0x0a) {
            offset += 1;
        }
        starts.push(offset as u32);
    }
    starts
}

impl LineTable {
    pub fn new(text: Text<'_>) -> LineTable {
        let starts = match text {
            Text::Latin1(bytes) => line_starts(text, bytes),
            Text::Utf16(units) => {
                let mut bytes = Vec::new();
                narrow(units, 0..units.len(), &mut bytes);
                line_starts(text, &bytes)
            }
        };
        let mut blocks = vec![Block {
            start: 0,
            first_line: 0,
        }];
        let mut block_start = 0;
        let offsets = (0..).zip(starts).map(|(line, start)| {
            u16::try_from(start - block_start).unwrap_or_else(|_| {
                block_start = start;
                blocks.push(Block {
                    start,
                    first_line: line,
                });
                0
            })
        });
        LineTable {
            offsets: offsets.collect(),
            blocks: blocks.into(),
        }
    }

    /// An offset past the end is on the last line.
    pub fn line_and_column(&self, offset: u32) -> (u32, u32) {
        let index = self.blocks.partition_point(|block| block.start <= offset) - 1;
        let block = &self.blocks[index];
        let end = (self.blocks.get(index + 1))
            .map_or(self.offsets.len(), |next| next.first_line as usize);
        let offsets = &self.offsets[block.first_line as usize..end];
        let in_block = offset - block.start;
        let line = offsets.partition_point(|&start| u32::from(start) <= in_block) - 1;
        (
            block.first_line + line as u32,
            in_block - u32::from(offsets[line]),
        )
    }

    fn start_of(&self, line: u32) -> Option<u32> {
        let offset = *self.offsets.get(line as usize)?;
        let index = (self.blocks).partition_point(|block| block.first_line <= line) - 1;
        Some(self.blocks[index].start + u32::from(offset))
    }

    /// Without what ends it. `text` is the one that the table was made for.
    pub fn range_of(&self, text: Text<'_>, line: u32) -> Option<Range<u32>> {
        let start = self.start_of(line)?;
        let Some(next) = self.start_of(line + 1) else {
            return Some(start..text.len() as u32);
        };
        let is_pair = next - start >= 2
            && text.at(next as usize - 1) == 0x0a
            && text.at(next as usize - 2) == 0x0d;
        Some(start..next - 1 - u32::from(is_pair))
    }

    /// A column past the end of its line is that end, and so is a line past the end of the text.
    pub fn offset_of(&self, text: Text<'_>, line: u32, column: u32) -> u32 {
        self.range_of(text, line)
            .map_or(text.len() as u32, |range| {
                range.start.saturating_add(column).min(range.end)
            })
    }
}
