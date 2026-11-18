//! Chooses the size of jumps and fills in the distances.

use super::assembler::{
    Assembler, CONDITIONAL_JUMP_SIZE, JUMP_SIZE, SHORT_CONDITIONAL_JUMP_OPCODE, SHORT_JUMP_OPCODE,
    SHORT_JUMP_SIZE, SiteKind, fill_with_nops, padding,
};
use crate::buffer::{CodeBuffer, Site};
use crate::label::{Code, CodeTooLarge, LabelState, Relocation, RelocationKind, resolve};

const JUMP_SAVING: u32 = JUMP_SIZE - SHORT_JUMP_SIZE;
const CONDITIONAL_JUMP_SAVING: u32 = CONDITIONAL_JUMP_SIZE - SHORT_JUMP_SIZE;

impl Assembler {
    pub fn finish(mut self) -> Result<Code, CodeTooLarge> {
        let end = self.new_label();
        self.bind(end);
        if self.buffer.has_data() {
            self.pad(4, 0, 15);
            self.buffer.put_data(|table| SiteKind::TableEntry { table });
        }
        self.buffer.check_size()?;
        if self.shorten_jumps {
            let removed = choose_sizes(&mut self.buffer);
            compact(&mut self.buffer, &removed);
        }
        let relocations = fill_distances(&mut self.buffer);
        let CodeBuffer {
            mut bytes,
            len,
            labels,
            alignment,
            ..
        } = self.buffer;
        bytes.truncate(len);
        let instructions_end = labels[end.0 as usize].offset;
        Ok(Code {
            bytes,
            labels,
            relocations,
            alignment,
            instructions_end,
        })
    }
}

fn shrinkable(kind: SiteKind) -> Option<(u32, SiteKind)> {
    match kind {
        SiteKind::Jump => Some((JUMP_SAVING, SiteKind::ShrunkJump)),
        SiteKind::ConditionalJump => {
            Some((CONDITIONAL_JUMP_SAVING, SiteKind::ShrunkConditionalJump))
        }
        _ => None,
    }
}

/// Returns for each site how many bytes go before it, and then the total.
///
/// Bytes only ever go, so a jump that reaches with what is known so far still reaches in the end.
fn choose_sizes(buffer: &mut CodeBuffer<SiteKind>) -> Vec<u32> {
    let CodeBuffer { sites, labels, .. } = buffer;
    let mut removed = vec![0; sites.len() + 1];

    let mut from_here_on = 0;
    for (index, site) in sites.iter_mut().enumerate().rev() {
        if let Some((saving, shrunk)) = shrinkable(site.kind) {
            let target = resolve(labels, site.label);
            if target.is_bound() && target.offset > site.at {
                let between = from_here_on - removed[target.link as usize];
                if target.offset - between - (site.at + SHORT_JUMP_SIZE + saving) <= i8::MAX as u32
                {
                    site.kind = shrunk;
                    from_here_on += saving;
                }
            }
        }
        removed[index] = from_here_on;
    }

    let mut before_here = 0;
    for (index, site) in sites.iter_mut().enumerate() {
        removed[index] = before_here;
        if let Some((_, shrunk)) = shrinkable(site.kind) {
            let target = resolve(labels, site.label);
            if target.is_bound() && target.offset <= site.at {
                let between = before_here - removed[target.link as usize];
                if site.at + SHORT_JUMP_SIZE - between - target.offset
                    <= i8::MIN.unsigned_abs().into()
                {
                    site.kind = shrunk;
                }
            }
        }
        before_here += match site.kind {
            SiteKind::ShrunkJump => JUMP_SAVING,
            SiteKind::ShrunkConditionalJump => CONDITIONAL_JUMP_SAVING,
            SiteKind::Padding { log2, bias, max } => {
                u32::from(max) - padding(site.at - before_here, log2, bias, max)
            }
            _ => 0,
        };
    }
    removed[sites.len()] = before_here;
    removed
}

/// Leaves only sites that are distances, and padding.
fn compact(buffer: &mut CodeBuffer<SiteKind>, removed: &[u32]) {
    let CodeBuffer {
        bytes,
        len,
        sites,
        labels,
        ..
    } = buffer;
    let mut unmoved = 0;
    for (index, site) in sites.iter_mut().enumerate() {
        let (old, before) = (site.at as usize, removed[index] as usize);
        let new = old - before;
        let saving = removed[index + 1] as usize - before;
        site.at = new as u32;
        match site.kind {
            SiteKind::Jump => {
                to_distance(site, JUMP_SIZE - 4, SiteKind::Distance32 { trailing: 0 })
            }
            SiteKind::ConditionalJump => {
                to_distance(
                    site,
                    CONDITIONAL_JUMP_SIZE - 4,
                    SiteKind::Distance32 { trailing: 0 },
                );
            }
            SiteKind::ShrunkJump | SiteKind::ShrunkConditionalJump => {
                let opcode = if site.kind == SiteKind::ShrunkJump {
                    SHORT_JUMP_OPCODE
                } else {
                    SHORT_CONDITIONAL_JUMP_OPCODE | bytes[old + 1] & 15
                };
                bytes.copy_within(unmoved..old, unmoved - before);
                bytes[new] = opcode;
                unmoved = old + SHORT_JUMP_SIZE as usize + saving;
                to_distance(site, 1, SiteKind::Distance8);
            }
            SiteKind::Padding { max, .. } if saving != 0 => {
                bytes.copy_within(unmoved..old, unmoved - before);
                fill_with_nops(&mut bytes[new..new + usize::from(max) - saving]);
                unmoved = old + usize::from(max);
            }
            _ => {}
        }
    }
    let total = removed[sites.len()] as usize;
    bytes.copy_within(unmoved..*len, unmoved - total);
    *len -= total;
    for label in labels.iter_mut().filter(|label| label.is_bound()) {
        label.offset -= removed[label.link as usize];
    }
}

fn to_distance(site: &mut Site<SiteKind>, opcode_size: u32, kind: SiteKind) {
    site.at += opcode_size;
    site.kind = kind;
}

fn fill_distances(buffer: &mut CodeBuffer<SiteKind>) -> Vec<Relocation> {
    let CodeBuffer {
        bytes,
        sites,
        labels,
        externals,
        ..
    } = buffer;
    let mut relocations = Vec::with_capacity(externals.len());
    for site in sites.iter() {
        let origin = match site.kind {
            SiteKind::Distance8 => site.at + 1,
            SiteKind::Distance32 { trailing } => site.at + 4 + u32::from(trailing),
            SiteKind::TableEntry { table } => labels[table.0 as usize].offset,
            SiteKind::Padding { .. } => continue,
            _ => unreachable!("the size of the jump is settled"),
        };
        let target = resolve(labels, site.label);
        if target.offset == LabelState::EXTERNAL {
            assert!(
                site.kind == SiteKind::Distance32 { trailing: 0 },
                "only jumps and calls leave the code"
            );
            relocations.push(Relocation {
                offset: site.at,
                kind: RelocationKind::Distance32,
                target: externals[target.link as usize],
            });
            continue;
        }
        assert!(target.is_bound(), "a label that is used is never bound");
        let distance = target.offset.wrapping_sub(origin) as i32;
        let at = site.at as usize;
        if site.kind == SiteKind::Distance8 {
            bytes[at] = i8::try_from(distance).expect("a short jump does not reach") as u8;
        } else {
            bytes[at..at + 4].copy_from_slice(&distance.to_le_bytes());
        }
    }
    relocations
}
