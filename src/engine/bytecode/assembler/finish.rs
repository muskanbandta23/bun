//! From words and labels to bytes and offsets.

use super::{
    Assembler, Header, MAX_WORDS, METADATA_SLOTS, NOWHERE, Operands, REFERENCED, TryRange,
};
use crate::code::{
    Body, CodeFlags, CodeKind, Handler, IntCases, StringSwitch, SuspendPoint, Tables, TooLarge,
};
use crate::instruction::{Opcode, Shape};
use crate::liveness::SavedRegisters;
use crate::metadata::MetadataKind;
use crate::operand::{Narrow, Operand, Wide16, Wide32, Width};
use crate::positions::Positions;

const MAX_INSTRUCTION_BYTES: usize = 32;
const MAX_LAYOUT_BYTES: usize = 4 * MetadataKind::COUNT;
const MAX_TARGETS: usize = 2;
const MAX_JUMPS_THREADED: usize = 4;
const MAX_SHRINKING_PASSES: usize = 3;

type InstructionBytes = [u8; MAX_INSTRUCTION_BYTES];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Size {
    Narrow,
    Wide16,
    Wide32,
}

impl Size {
    fn of_distance(distance: i64) -> Size {
        if i8::try_from(distance).is_ok() {
            Size::Narrow
        } else if i16::try_from(distance).is_ok() {
            Size::Wide16
        } else {
            Size::Wide32
        }
    }

    fn bytes(self, header: Header) -> u32 {
        let opcode = header.opcode();
        let bytes = match self {
            Size::Narrow => opcode.size::<Narrow>(),
            Size::Wide16 => opcode.size::<Wide16>(),
            Size::Wide32 => opcode.size::<Wide32>(),
        };
        bytes as u32
    }
}

/// Whether the operands fit, but for the targets.
#[inline(always)]
fn fits<W: Width>(header: Header, operands: &Operands) -> bool {
    let half = 1u32 << (8 * W::OPERAND_BYTES - 1);
    let mut fits = true;
    for (index, &word) in operands[..header.operands()].iter().enumerate() {
        let is_signed = header.0 >> (Header::SIGNED as usize + index) & 1 != 0;
        let is_source = header.0 >> (Header::SOURCE as usize + index) & 1 != 0;
        fits &= match (is_signed, is_source) {
            (false, false) => word < 2 * half,
            (false, true) => true,
            (true, false) => word.wrapping_add(half) < 2 * half,
            (true, true) => match Operand::from_word(word).as_constant() {
                Some(constant) => constant < half - W::FIRST_CONSTANT as u32,
                None => word.wrapping_add(half) < half + W::FIRST_CONSTANT as u32,
            },
        };
    }
    fits
}

/// Returns the size. What it stores for a target means nothing.
#[inline(always)]
fn put<W: Width>(
    header: Header,
    operands: &Operands,
    metadata: u32,
    bytes: &mut InstructionBytes,
) -> usize {
    bytes[0] = if W::OPERAND_BYTES == 2 {
        Opcode::Wide16Prefix as u8
    } else {
        Opcode::Wide32Prefix as u8
    };
    bytes[W::PREFIX_BYTES] = header.0 as u8;
    let mut at = W::PREFIX_BYTES + 1;
    for (index, &word) in operands[..header.operands()].iter().enumerate() {
        let is_source = header.0 >> (Header::SOURCE as usize + index) & 1 != 0;
        let stored = match Operand::from_word(word).as_constant() {
            Some(constant) if is_source => constant + W::FIRST_CONSTANT as u32,
            _ => word,
        };
        bytes[at..at + 4].copy_from_slice(&stored.to_le_bytes());
        at += W::OPERAND_BYTES;
    }
    bytes[at..at + 4].copy_from_slice(&metadata.to_le_bytes());
    if header.metadata() != Header::NO_METADATA {
        at += W::METADATA_BYTES;
    }
    at
}

#[inline(never)]
fn put_wide(
    size: Size,
    header: Header,
    operands: &Operands,
    metadata: u32,
    bytes: &mut InstructionBytes,
) -> usize {
    match size {
        Size::Wide32 => put::<Wide32>(header, operands, metadata, bytes),
        _ => put::<Wide16>(header, operands, metadata, bytes),
    }
}

fn put_distance(size: Size, index: usize, distance: u32, instruction: &mut [u8]) {
    fn put<W: Width>(index: usize, distance: u32, instruction: &mut [u8]) {
        let at = W::PREFIX_BYTES + 1 + index * W::OPERAND_BYTES;
        instruction[at..at + W::OPERAND_BYTES]
            .copy_from_slice(&distance.to_le_bytes()[..W::OPERAND_BYTES]);
    }
    match size {
        Size::Narrow => put::<Narrow>(index, distance, instruction),
        Size::Wide16 => put::<Wide16>(index, distance, instruction),
        Size::Wide32 => put::<Wide32>(index, distance, instruction),
    }
}

/// An instruction with a target.
struct Branch {
    header: Header,
    site: u32,
    metadata: u32,
    size: Size,
}

/// A branch that may not reach its targets at the size that its other operands need.
struct FarBranch {
    branch: u32,
    /// In the draft.
    offset: u32,
    size: Size,
    growth: u32,
    /// In the draft, each with the number of far branches before it.
    targets: [(u32, u32); MAX_TARGETS],
}

/// The draft has every branch at the size that its other operands need.
#[derive(Default)]
pub(super) struct Encoder {
    bytes: Vec<u8>,
    branches: Vec<Branch>,
    far_branches: Vec<FarBranch>,
    /// For each far branch and for the end: the offset in the draft, and how far what is there moves.
    shifts: Vec<(u32, u32)>,
    /// The sites of the instructions that suspend and resume, and the index that the point was added with.
    suspend_points: Vec<(u32, u32, u32)>,
    saved_registers: SavedRegisters,
}

impl Encoder {
    fn reserve(&mut self, size: usize) -> Result<(), TooLarge> {
        if let Some(more) = size.checked_sub(self.bytes.len()) {
            self.bytes.try_reserve(more).map_err(|_| TooLarge)?;
            self.bytes.resize(self.bytes.capacity(), 0);
        }
        Ok(())
    }

    #[inline]
    fn moved(&self, draft_offset: u32) -> u32 {
        if self.shifts.is_empty() {
            return draft_offset;
        }
        let before = self
            .shifts
            .partition_point(|&(offset, _)| offset < draft_offset);
        draft_offset + self.shifts[before].1
    }

    fn sum_growth(&mut self) -> Result<u32, TooLarge> {
        self.shifts.clear();
        let mut shift = 0u32;
        for far in &self.far_branches {
            self.shifts.push((far.offset, shift));
            shift = shift.checked_add(far.growth).ok_or(TooLarge)?;
        }
        self.shifts.push((u32::MAX, shift));
        Ok(shift)
    }

    /// Distances only get shorter, so any pass can be the last.
    fn shrink(&mut self) -> bool {
        let mut has_changed = false;
        for (far, &(_, shift)) in self.far_branches.iter_mut().zip(&self.shifts) {
            let branch = &self.branches[far.branch as usize];
            let least = branch.size.bytes(branch.header);
            let from = i64::from(far.offset) + i64::from(shift);
            for size in [Size::Narrow, Size::Wide16] {
                if size < branch.size || size >= far.size {
                    continue;
                }
                let growth = size.bytes(branch.header) - least;
                let reaches = far.targets.iter().all(|&(target, before)| {
                    let to = i64::from(target) + i64::from(self.shifts[before as usize].1);
                    let saved = if target > far.offset {
                        far.growth - growth
                    } else {
                        0
                    };
                    Size::of_distance(to - from - i64::from(saved)) <= size
                });
                if reaches {
                    (far.size, far.growth) = (size, growth);
                    has_changed = true;
                    break;
                }
            }
        }
        has_changed
    }
}

struct Layout {
    kinds: u64,
    /// Where the next entry of each kind goes.
    next: [u32; METADATA_SLOTS],
    words: u32,
}

static ENTRY_WORDS: [u8; METADATA_SLOTS] = {
    let mut words = [0; METADATA_SLOTS];
    let mut index = 0;
    while index < MetadataKind::COUNT {
        words[index] = MetadataKind::ALL[index].words() as u8;
        index += 1;
    }
    words
};

impl Layout {
    fn new(counts: &[u32; METADATA_SLOTS]) -> Result<Layout, TooLarge> {
        let mut layout = Layout {
            kinds: 0,
            next: [0; METADATA_SLOTS],
            words: 0,
        };
        let mut words = 0u64;
        for index in 0..MetadataKind::COUNT {
            layout.next[index] = words as u32;
            layout.kinds |= u64::from(counts[index] != 0) << index;
            words += u64::from(counts[index]) * u64::from(ENTRY_WORDS[index]);
        }
        layout.words = u32::try_from(words).map_err(|_| TooLarge)?;
        Ok(layout)
    }

    #[inline(always)]
    fn take(&mut self, header: Header) -> u32 {
        let next = &mut self.next[header.metadata()];
        let entry = *next;
        *next += u32::from(ENTRY_WORDS[header.metadata()]);
        entry
    }

    fn is_wide(&self) -> bool {
        self.words > u32::from(u16::MAX)
    }

    /// Once all entries are taken. Returns the size.
    fn write(&self, bytes: &mut [u8]) -> usize {
        let step = if self.is_wide() { 4 } else { 2 };
        let (mut rest, mut at) = (self.kinds, 0);
        while rest != 0 {
            let end = self.next[rest.trailing_zeros() as usize % METADATA_SLOTS];
            bytes[at..at + 4].copy_from_slice(&end.to_le_bytes());
            at += step;
            rest &= rest - 1;
        }
        at
    }
}

impl Assembler {
    /// Nothing but `reset` follows.
    pub(crate) fn finish(
        &mut self,
        kind: CodeKind,
        flags: CodeFlags,
        locals: u32,
        parameters: u32,
    ) -> Result<Body, TooLarge> {
        debug_assert!(self.tries.is_empty() && self.suspended_tries.is_empty());
        if self.is_too_large
            || locals.max(parameters) > Body::MAX_FRAME_WORDS
            || (self.len as usize + MAX_WORDS >= self.words.len() && !self.grow())
        {
            return Err(TooLarge);
        }
        let mut layout = Layout::new(&self.metadata_counts)?;
        let jumps_are_bypassed = self.thread_jumps();
        let mut size = self.draft(&mut layout, jumps_are_bypassed)?;
        if !self.put_distances() {
            size = self.widen_far_branches(size)?;
            let all_reach = self.put_distances();
            debug_assert!(all_reach);
        }

        let mut end = size as usize;
        let rest = MAX_LAYOUT_BYTES + Positions::max_size(self.positions.len());
        self.encoder.reserve(end + rest)?;
        for index in 0..self.positions.len() {
            self.positions[index].0 = self.offset_of(self.positions[index].0);
        }
        end += layout.write(&mut self.encoder.bytes[end..]);
        end += Positions::encode(
            self.positions.iter().copied(),
            &mut self.encoder.bytes[end..],
        );
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(end).map_err(|_| TooLarge)?;
        bytes.extend_from_slice(&self.encoder.bytes[..end]);

        let mut body = Body {
            bytes: bytes.into(),
            tables: self.tables(),
            metadata_kinds: layout.kinds,
            instructions_size: size,
            locals,
            parameters,
            kind,
            flags,
            has_wide_layout: layout.is_wide(),
        };
        if !body.suspend_points().is_empty() {
            self.encoder.saved_registers.fill_in(&mut body)?;
        }
        Ok(body)
    }

    /// Moves the labels of each `jump` to its target. If that works for all, a `jump` is only reached from above.
    fn thread_jumps(&mut self) -> bool {
        self.words[self.len as usize] = Header::PADDING.0;
        let mut jumps_are_bypassed = true;
        for index in 0..self.labels.len() {
            let state = self.labels[index];
            debug_assert!(state != NOWHERE | REFERENCED, "not bound");
            let mut site = state & !REFERENCED;
            if state & REFERENCED != 0 {
                let mut jumps = 0;
                while self.header(site) == Header::JUMP {
                    if jumps == MAX_JUMPS_THREADED {
                        jumps_are_bypassed = false;
                        break;
                    }
                    site = self.labels[self.words[site as usize + 1] as usize] & !REFERENCED;
                    jumps += 1;
                }
            }
            self.labels[index] = state & REFERENCED | site;
        }
        jumps_are_bypassed
    }

    #[inline]
    fn site_of(&self, label: u32) -> u32 {
        self.labels[label as usize] & !REFERENCED
    }

    /// Replaces each header with the offset of the instruction in the draft. Returns the size.
    fn draft(&mut self, layout: &mut Layout, jumps_are_bypassed: bool) -> Result<u32, TooLarge> {
        self.encoder.branches.clear();
        self.encoder.shifts.clear();
        self.encoder.suspend_points.clear();
        let (mut site, mut offset) = (0, 0);
        let mut previous_stops = false;
        while site < self.len as usize {
            if offset + MAX_INSTRUCTION_BYTES > self.encoder.bytes.len() {
                if offset > Body::MAX_SIZE as usize {
                    return Err(TooLarge);
                }
                let words_left = self.len as usize - site;
                self.encoder
                    .reserve(offset + 2 * words_left + MAX_INSTRUCTION_BYTES)?;
            }
            let header = Header(self.words[site]);
            self.words[site] = offset as u32;
            let next = site + 1 + header.operands();
            if header.has(Header::SPECIAL) {
                if header == Header::PADDING
                    || (header == Header::JUMP && previous_stops && jumps_are_bypassed)
                {
                    site = next;
                    continue;
                }
                if header == Header::JUMP && self.jumps_over_nothing(site, jumps_are_bypassed) {
                    let target = self.site_of(self.words[site + 1]) as usize;
                    self.words[site..target].fill(offset as u32);
                    previous_stops = false;
                    site = target;
                    continue;
                }
                self.number_suspend_point(header, site);
            }
            let operands = self.words[site + 1..].first_chunk().expect("grown");
            let bytes = self.encoder.bytes[offset..]
                .first_chunk_mut()
                .expect("reserved");
            let metadata = layout.take(header);
            let size = if metadata > u32::from(u16::MAX) {
                Size::Wide32
            } else if fits::<Narrow>(header, operands) {
                Size::Narrow
            } else if fits::<Wide16>(header, operands) {
                Size::Wide16
            } else {
                Size::Wide32
            };
            offset += match size {
                Size::Narrow => put::<Narrow>(header, operands, metadata, bytes),
                _ => put_wide(size, header, operands, metadata, bytes),
            };
            if header.targets().next().is_some() {
                self.encoder.branches.push(Branch {
                    header,
                    site: site as u32,
                    metadata,
                    size,
                });
            }
            previous_stops = header.has(Header::STOPS);
            site = next;
        }
        if offset > Body::MAX_SIZE as usize {
            return Err(TooLarge);
        }
        self.words[site] = offset as u32;
        Ok(offset as u32)
    }

    fn jumps_over_nothing(&self, site: usize, jumps_are_bypassed: bool) -> bool {
        let target = self.site_of(self.words[site + 1]);
        let mut next = site as u32 + 2;
        while next < target {
            next += match self.header(next) {
                Header::PADDING => 1,
                Header::JUMP if jumps_are_bypassed => 2,
                _ => return false,
            };
        }
        next == target
    }

    fn number_suspend_point(&mut self, header: Header, site: usize) {
        let opcode = header.opcode();
        if opcode.shapes().last() != Some(&Shape::SuspendPoint) {
            return;
        }
        let points = &mut self.encoder.suspend_points;
        let operand = &mut self.words[site + header.operands()];
        if opcode == Opcode::Resume {
            let (_, resume, point) = points.last_mut().expect("follows what suspends");
            debug_assert!(*point == *operand);
            *resume = site as u32;
        } else {
            points.push((site as u32, NOWHERE, *operand));
        }
        *operand = points.len() as u32 - 1;
    }

    #[inline]
    fn offset_of(&self, site: u32) -> u32 {
        self.encoder.moved(self.words[site as usize])
    }

    #[inline]
    fn target_of(&self, label: u32) -> u32 {
        self.offset_of(self.site_of(label))
    }

    /// Whether all fit.
    fn put_distances(&mut self) -> bool {
        for branch in 0..self.encoder.branches.len() {
            let Branch {
                header, site, size, ..
            } = self.encoder.branches[branch];
            let from = self.offset_of(site);
            for index in header.targets() {
                let to = self.target_of(self.words[site as usize + 1 + index]);
                let distance = to.wrapping_sub(from);
                if Size::of_distance(i64::from(distance as i32)) > size {
                    return false;
                }
                let instruction = &mut self.encoder.bytes[from as usize..];
                put_distance(size, index, distance, instruction);
            }
        }
        true
    }

    /// Returns the new size.
    #[cold]
    fn widen_far_branches(&mut self, draft_size: u32) -> Result<u32, TooLarge> {
        // An instruction takes at most twice the bytes as `Wide16` and four times as `Wide32`.
        let max_growth = if draft_size <= 1 << 14 { 2 } else { 4 };
        self.encoder.far_branches.clear();
        for (index, branch) in self.encoder.branches.iter().enumerate() {
            let offset = self.words[branch.site as usize];
            let mut targets = [(offset, 0); MAX_TARGETS];
            let mut size = branch.size;
            for (target, operand) in targets.iter_mut().zip(branch.header.targets()) {
                target.0 = self.target_of(self.words[branch.site as usize + 1 + operand]);
                let distance = i64::from(target.0) - i64::from(offset);
                size = size.max(Size::of_distance(distance * max_growth));
            }
            if size > branch.size {
                self.encoder.far_branches.push(FarBranch {
                    branch: index as u32,
                    offset,
                    size,
                    growth: size.bytes(branch.header) - branch.size.bytes(branch.header),
                    targets,
                });
            }
        }

        let encoder = &mut self.encoder;
        encoder.sum_growth()?;
        for far in &mut encoder.far_branches {
            for (target, before) in &mut far.targets {
                *before = encoder
                    .shifts
                    .partition_point(|&(offset, _)| offset < *target)
                    as u32;
            }
        }
        for _ in 0..MAX_SHRINKING_PASSES {
            if !encoder.shrink() {
                break;
            }
            encoder.sum_growth()?;
        }
        encoder.far_branches.retain(|far| far.growth != 0);
        let size = draft_size
            .checked_add(encoder.sum_growth()?)
            .filter(|&size| size <= Body::MAX_SIZE)
            .ok_or(TooLarge)?;

        encoder.reserve(size as usize)?;
        let mut end = draft_size as usize;
        for (far, &(offset, shift)) in encoder.far_branches.iter().zip(&encoder.shifts).rev() {
            let branch = &mut encoder.branches[far.branch as usize];
            let rest = (offset + branch.size.bytes(branch.header)) as usize;
            let start = (offset + shift) as usize;
            let operands = self.words[branch.site as usize + 1..]
                .first_chunk()
                .expect("grown");
            let mut bytes = InstructionBytes::default();
            let size = put_wide(
                far.size,
                branch.header,
                operands,
                branch.metadata,
                &mut bytes,
            );
            encoder.bytes.copy_within(rest..end, start + size);
            encoder.bytes[start..start + size].copy_from_slice(&bytes[..size]);
            branch.size = far.size;
            end = offset as usize;
        }
        Ok(size)
    }

    fn handlers(&mut self) -> Box<[Handler]> {
        let mut ranges = core::mem::take(&mut self.try_ranges);
        ranges.retain(|range| self.is_referenced(range.handler));
        for range in &mut ranges {
            (range.start, range.end) = (self.offset_of(range.start), self.offset_of(range.end));
        }
        // Those of one depth are disjoint and ascend.
        ranges.sort_by_key(|range| core::cmp::Reverse(range.depth));
        ranges.dedup_by(|next: &mut TryRange, range: &mut TryRange| {
            let continues = (range.handler, range.kind, range.depth, range.end)
                == (next.handler, next.kind, next.depth, next.start);
            if continues {
                range.end = next.end;
            }
            continues
        });
        let handlers = ranges.iter().map(|range| Handler {
            range: range.start..range.end,
            target: self.target_of(range.handler.0),
            kind: range.kind,
        });
        let handlers = handlers.collect();
        self.try_ranges = ranges;
        handlers
    }

    fn tables(&mut self) -> Option<Box<Tables>> {
        let handlers = self.handlers();
        if handlers.is_empty()
            && self.int_switches.is_empty()
            && self.string_switches.is_empty()
            && self.spread_masks.is_empty()
            && self.encoder.suspend_points.is_empty()
            && self.coverage_blocks.is_empty()
            && self.scope_registers.is_empty()
        {
            return None;
        }
        let mut int_switches: Box<[_]> = self.int_switches.drain(..).collect();
        for switch in &mut int_switches {
            switch.default = self.target_of(switch.default);
            match &mut switch.cases {
                IntCases::Dense { targets, .. } => targets
                    .iter_mut()
                    .for_each(|target| *target = self.target_of(*target)),
                IntCases::Sparse(cases) => cases
                    .iter_mut()
                    .for_each(|(_, target)| *target = self.target_of(*target)),
            }
        }
        let mut string_switches: Box<[_]> = self.string_switches.drain(..).collect();
        for StringSwitch { default, targets } in &mut string_switches {
            for target in targets.iter_mut().chain([default]) {
                *target = match *target {
                    NOWHERE => 0,
                    label => self.target_of(label),
                };
            }
        }
        let suspend_points = self.encoder.suspend_points.iter();
        let suspend_points = suspend_points.map(|&(suspend, resume, point)| SuspendPoint {
            suspend: self.offset_of(suspend),
            resume: self.offset_of(resume),
            kind: self.suspend_kinds[point as usize],
            registers: 0..0,
        });
        Some(Box::new(Tables {
            suspend_points: suspend_points.collect(),
            handlers,
            int_switches,
            string_switches,
            spread_masks: self.spread_masks.drain(..).collect(),
            saved_registers: Box::default(),
            coverage_blocks: self.coverage_blocks.as_slice().into(),
            scope_registers: (self.scope_registers.iter())
                .map(|&(site, register)| (self.offset_of(site), register))
                .collect(),
        }))
    }
}
