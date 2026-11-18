//! Collects instructions as words, to be encoded once all of them are known.

mod finish;

use bun_engine_syntax::EvalContext;

use crate::code::{CoverageBlock, HandlerKind, IntCases, IntSwitch, StringSwitch};
use crate::effects::for_each_operand_def;
use crate::instruction::{Flow, Opcode, Shape, field_type};
use crate::operand::*;
use crate::positions::Position;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Label(u32);

impl Label {
    /// As an operand for [`Assembler::replace`] and [`Assembler::set_operand`].
    pub(crate) fn word(self) -> u32 {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn from_word(word: u32) -> Label {
        Label(word)
    }
}

/// Where an instruction starts, or will start.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct Site(u32);

/// The first word of an instruction: its opcode and what the encoder needs of its description.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Header(u32);

impl Header {
    const OPERANDS: u32 = 8;
    const METADATA: u32 = 11;
    /// A bit for each operand. With `SOURCE` it is a `Src`.
    const SIGNED: u32 = 17;
    /// Without `SIGNED` it is a `Target`.
    const SOURCE: u32 = 23;
    const THROWS: u32 = 1 << 29;
    const STOPS: u32 = 1 << 30;
    /// Has a target or a suspend point, or is padding.
    const SPECIAL: u32 = 1 << 31;

    const NO_METADATA: usize = METADATA_SLOTS - 1;
    /// Stands for a word that `replace` freed.
    const PADDING: Header = Header(Header::of(Opcode::Wide16Prefix).0 | Header::SPECIAL);
    const JUMP: Header = Header::of(Opcode::Goto);

    const fn of(opcode: Opcode) -> Header {
        let shapes = opcode.shapes();
        let metadata = match opcode.metadata() {
            Some(kind) => kind as u32,
            None => Self::NO_METADATA as u32,
        };
        let mut word =
            opcode as u32 | (shapes.len() as u32) << Self::OPERANDS | metadata << Self::METADATA;
        let mut index = 0;
        while index < shapes.len() {
            let (signed, source) = (
                1 << (Self::SIGNED + index as u32),
                1 << (Self::SOURCE + index as u32),
            );
            word |= match shapes[index] {
                Shape::Src => signed | source,
                Shape::Target => source | Self::SPECIAL,
                Shape::SuspendPoint => Self::SPECIAL,
                shape if shape.is_signed() => signed,
                _ => 0,
            };
            index += 1;
        }
        if opcode.flow().can_throw() {
            word |= Self::THROWS;
        }
        if matches!(opcode.flow(), Flow::Jumps | Flow::Ends) {
            word |= Self::STOPS;
        }
        Header(word)
    }

    #[inline(always)]
    fn opcode(self) -> Opcode {
        Opcode::ALL[usize::from(self.0 as u8)]
    }

    #[inline(always)]
    fn operands(self) -> usize {
        (self.0 >> Self::OPERANDS & 7) as usize
    }

    #[inline(always)]
    fn metadata(self) -> usize {
        (self.0 >> Self::METADATA) as usize % METADATA_SLOTS
    }

    #[inline(always)]
    fn has(self, flag: u32) -> bool {
        self.0 & flag != 0
    }

    /// The indices of the operands that are targets.
    #[inline(always)]
    fn targets(self) -> impl Iterator<Item = usize> {
        let mut rest = self.0 >> Self::SOURCE & !(self.0 >> Self::SIGNED) & 0x3f;
        core::iter::from_fn(move || {
            let index = (rest != 0).then(|| rest.trailing_zeros() as usize)?;
            rest &= rest - 1;
            Some(index)
        })
    }
}

static HEADERS: [Header; Opcode::COUNT] = {
    let mut headers = [Header::PADDING; Opcode::COUNT];
    let mut index = 0;
    while index < Opcode::COUNT {
        headers[index] = Header::of(Opcode::ALL[index]);
        index += 1;
    }
    headers
};

type Operands = [u32; Opcode::MAX_OPERANDS];

const MAX_WORDS: usize = 1 + Opcode::MAX_OPERANDS;
const METADATA_SLOTS: usize = 64;
const RETAINED_WORDS: usize = 1 << 16;
const NOWHERE: u32 = i32::MAX as u32;
const REFERENCED: u32 = 1 << 31;
const MAX_DENSE_RANGE: i64 = 1000;
const MAX_DENSE_RANGE_PER_CASE: i64 = 15;
const MASKS_COMPARED: usize = 8;

struct OpenTry {
    handler: Label,
    kind: HandlerKind,
    start: u32,
    /// Whether anything since `start` does, in an inner try or not.
    throws: bool,
}

struct TryRange {
    handler: Label,
    kind: HandlerKind,
    depth: u32,
    start: u32,
    end: u32,
}

#[must_use]
pub(crate) struct SuspendedTries {
    count: u32,
    at: u32,
}

pub(crate) struct Assembler {
    /// Longer than `len`, so that an instruction is stored without a check for each word.
    words: Vec<u32>,
    len: u32,
    last: u32,
    is_reachable: bool,
    is_too_large: bool,
    records_positions: bool,
    /// `Header::THROWS` while the next instruction that can throw is news to the positions or the tries.
    watched: u32,
    /// The end of the header of the last instruction that was news.
    noticed_throw: u32,
    discarded: Operands,
    metadata_counts: [u32; METADATA_SLOTS],
    /// Where each is bound, or `NOWHERE`, and `REFERENCED`.
    labels: Vec<u32>,
    pending_position: Option<Position>,
    positions: Vec<(u32, Position)>,
    tries: Vec<OpenTry>,
    suspended_tries: Vec<OpenTry>,
    try_ranges: Vec<TryRange>,
    /// With labels for targets.
    int_switches: Vec<IntSwitch>,
    /// With labels for targets, or `NOWHERE` if unreachable code added it.
    string_switches: Vec<StringSwitch>,
    spread_masks: Vec<Box<[u32]>>,
    mask: Vec<u32>,
    suspend_kinds: Vec<SuspendKind>,
    coverage_blocks: Vec<CoverageBlock>,
    scope_registers: Vec<(u32, Register)>,
    encoder: finish::Encoder,
}

impl Default for Assembler {
    fn default() -> Assembler {
        Assembler {
            words: Vec::new(),
            len: 0,
            last: NOWHERE,
            is_reachable: true,
            is_too_large: false,
            records_positions: true,
            watched: 0,
            noticed_throw: 0,
            discarded: Operands::default(),
            metadata_counts: [0; METADATA_SLOTS],
            labels: Vec::new(),
            pending_position: None,
            positions: Vec::new(),
            tries: Vec::new(),
            suspended_tries: Vec::new(),
            try_ranges: Vec::new(),
            int_switches: Vec::new(),
            string_switches: Vec::new(),
            spread_masks: Vec::new(),
            mask: Vec::new(),
            suspend_kinds: Vec::new(),
            coverage_blocks: Vec::new(),
            scope_registers: Vec::new(),
            encoder: finish::Encoder::default(),
        }
    }
}

macro_rules! parameter_type {
    (Target) => { Label };
    (Src) => { impl Into<Operand> };
    ($shape:ident) => { field_type!($shape) };
}

macro_rules! operand_word {
    ($assembler:ident, Target, $operand:ident) => {
        $assembler.refer_to($operand)
    };
    ($assembler:ident, Src, $operand:ident) => {
        $operand.into().word()
    };
    ($assembler:ident, $shape:ident, $operand:ident) => {
        $operand.word()
    };
}

macro_rules! define_emitters {
    (
        $(#[$wide16_doc:meta])* Wide16Prefix wide16_prefix Pure {};
        $(#[$wide32_doc:meta])* Wide32Prefix wide32_prefix Pure {};
        $(
            $(#[$doc:meta])*
            $name:ident $mnemonic:ident $flow:ident { $($field:ident: $shape:ident),* } $(=> $metadata:ident)?;
        )*
    ) => {
        impl Assembler {$(
            $(#[$doc])*
            #[inline]
            pub(crate) fn $mnemonic(&mut self, $($field: parameter_type!($shape)),*) {
                const COUNT: usize = Opcode::$name.shapes().len();
                let words: [u32; COUNT] = [$(operand_word!(self, $shape, $field)),*];
                self.emit(const { Header::of(Opcode::$name) })[..COUNT].copy_from_slice(&words);
            }
        )*}
    };
}

crate::instruction_table!(define_emitters);

impl Assembler {
    pub(crate) fn reset(&mut self) {
        if self.words.len() > RETAINED_WORDS {
            *self = Assembler::default();
            return;
        }
        self.len = 0;
        self.last = NOWHERE;
        self.is_reachable = true;
        self.is_too_large = false;
        self.records_positions = true;
        self.watched = 0;
        self.noticed_throw = 0;
        self.metadata_counts = [0; METADATA_SLOTS];
        self.labels.clear();
        self.pending_position = None;
        self.positions.clear();
        self.tries.clear();
        self.suspended_tries.clear();
        self.try_ranges.clear();
        self.int_switches.clear();
        self.string_switches.clear();
        self.spread_masks.clear();
        self.suspend_kinds.clear();
        self.coverage_blocks.clear();
        self.scope_registers.clear();
    }

    /// Returns where the operands go.
    #[inline(never)]
    fn emit(&mut self, header: Header) -> &mut Operands {
        let at = self.len;
        if !self.is_reachable || (at as usize + MAX_WORDS >= self.words.len() && !self.grow()) {
            return &mut self.discarded;
        }
        if header.has(self.watched) {
            self.notice_throw(at);
        }
        self.metadata_counts[header.metadata()] += 1;
        self.last = at;
        self.len = at + 1 + header.operands() as u32;
        self.is_reachable = !header.has(Header::STOPS);
        let [first, operands @ ..] = self.words[at as usize..]
            .first_chunk_mut::<MAX_WORDS>()
            .expect("grown");
        *first = header.0;
        operands
    }

    /// If it cannot, no code is reachable from here on.
    #[cold]
    fn grow(&mut self) -> bool {
        let more = self.words.len().max(256);
        let len = self.words.len() + more;
        if len >= NOWHERE as usize || self.words.try_reserve_exact(more).is_err() {
            self.is_too_large = true;
            self.is_reachable = false;
            return false;
        }
        self.words.resize(len, 0);
        true
    }

    #[inline]
    fn notice_throw(&mut self, at: u32) {
        self.watched = 0;
        self.noticed_throw = at + 1;
        if let Some(open) = self.tries.last_mut() {
            open.throws = true;
            self.labels[open.handler.0 as usize] |= REFERENCED;
        }
        if let Some(position) = self.pending_position
            && self.positions.last().is_none_or(|last| last.1 != position)
        {
            self.positions.push((at, position));
        }
    }

    // Labels

    #[inline]
    pub(crate) fn label(&mut self) -> Label {
        self.labels.push(NOWHERE);
        Label(self.labels.len() as u32 - 1)
    }

    #[inline]
    fn refer_to(&mut self, label: Label) -> u32 {
        self.labels[label.0 as usize] |= u32::from(self.is_reachable) * REFERENCED;
        label.0
    }

    pub(crate) fn bind(&mut self, label: Label) {
        self.is_reachable |= self.is_referenced(label) && !self.is_too_large;
        self.bind_loop_head(label);
    }

    pub(crate) fn bind_loop_head(&mut self, label: Label) {
        self.last = NOWHERE;
        let state = &mut self.labels[label.0 as usize];
        debug_assert!(*state & !REFERENCED == NOWHERE, "bound twice");
        if self.is_reachable {
            *state = *state & REFERENCED | self.len;
        }
    }

    #[inline]
    pub(crate) fn is_reachable(&self) -> bool {
        self.is_reachable
    }

    #[inline]
    pub(crate) fn is_referenced(&self, label: Label) -> bool {
        self.labels[label.0 as usize] & REFERENCED != 0
    }

    // Positions

    #[inline]
    pub(crate) fn position(&mut self, divot: u32, start: u32, end: u32) {
        if self.records_positions {
            self.pending_position = Some(Position { divot, start, end });
            self.watched = Header::THROWS;
        }
    }

    pub(crate) fn set_records_positions(&mut self, records_positions: bool) {
        self.records_positions = records_positions;
    }

    /// The last that was set, recorded by now or not.
    // Instructions already emitted

    #[inline]
    pub(crate) fn here(&self) -> Site {
        Site(self.len)
    }

    #[inline]
    fn header(&self, site: u32) -> Header {
        Header(self.words[site as usize])
    }

    /// None after a label or the boundary of a try.
    #[inline]
    pub(crate) fn last(&self) -> Option<(Site, Opcode)> {
        (self.last != NOWHERE && self.is_reachable)
            .then(|| (Site(self.last), self.header(self.last).opcode()))
    }

    pub(crate) fn opcode(&self, site: Site) -> Opcode {
        self.header(site.0).opcode()
    }

    #[inline]
    pub(crate) fn operand(&self, site: Site, index: usize) -> u32 {
        debug_assert!(site.0 < self.len && index < self.header(site.0).operands());
        self.words[site.0 as usize + 1 + index]
    }

    #[inline]
    pub(crate) fn set_operand(&mut self, site: Site, index: usize, word: u32) {
        debug_assert!(site.0 < self.len && index < self.header(site.0).operands());
        self.words[site.0 as usize + 1 + index] = word;
        if self.header(site.0).targets().any(|target| target == index) {
            self.labels[word as usize] |= REFERENCED;
        }
    }

    /// Only the last instruction may come to throw, or change whether control goes on to the next.
    pub(crate) fn replace(&mut self, site: Site, opcode: Opcode, words: &[u32]) {
        let (old, new) = (self.header(site.0), HEADERS[opcode as usize]);
        let start = site.0 as usize;
        let mut end = start + 1 + old.operands();
        while end < self.len as usize && self.words[end] == Header::PADDING.0 {
            end += 1;
        }
        assert!(words.len() == new.operands());
        let (operands, padding) = self.words[start + 1..end].split_at_mut(words.len());
        operands.copy_from_slice(words);
        padding.fill(Header::PADDING.0);
        // A position or a try may be there for it, so it is not left out even as a `jump`.
        self.words[start] = new.0 | old.0 & Header::THROWS;
        self.metadata_counts[old.metadata()] -= 1;
        self.metadata_counts[new.metadata()] += 1;
        for index in new.targets() {
            self.labels[words[index] as usize] |= REFERENCED;
        }
        if site.0 != self.last {
            debug_assert!(old.has(Header::THROWS) || !new.has(Header::THROWS));
            debug_assert!(old.has(Header::STOPS) == new.has(Header::STOPS));
            return;
        }
        if new.has(self.watched) && !old.has(Header::THROWS) {
            self.notice_throw(site.0);
        }
        self.is_reachable = !new.has(Header::STOPS) && !self.is_too_large;
    }

    pub(crate) fn writes_register(&self, from: Site, register: Register) -> bool {
        let mut at = from.0 as usize;
        while at < self.len as usize {
            let header = Header(self.words[at]);
            let operands = &self.words[at + 1..][..header.operands()];
            at += 1 + operands.len();
            let mut writes = false;
            for_each_operand_def(header.opcode(), operands, |run, _| {
                writes |= run.contains(register);
            });
            if writes {
                return true;
            }
        }
        false
    }

    // Tries

    pub(crate) fn push_try(&mut self, handler: Label, kind: HandlerKind) {
        self.tries.push(OpenTry {
            handler,
            kind,
            start: self.len,
            throws: false,
        });
        self.note_try_boundary();
    }

    fn note_try_boundary(&mut self) {
        self.watched = Header::THROWS;
        self.last = NOWHERE;
    }

    pub(crate) fn pop_try(&mut self) {
        self.close_try();
    }

    fn close_try(&mut self) -> OpenTry {
        let open = self.tries.pop().expect("a try is open");
        self.note_try_boundary();
        if open.throws {
            self.try_ranges.push(TryRange {
                handler: open.handler,
                kind: open.kind,
                depth: self.tries.len() as u32,
                start: open.start,
                end: self.len,
            });
            if let Some(outer) = self.tries.last_mut() {
                outer.throws = true;
            }
        }
        open
    }

    pub(crate) fn try_depth(&self) -> u32 {
        self.tries.len() as u32
    }

    /// Resumed in the opposite order.
    pub(crate) fn suspend_tries(&mut self, down_to_depth: u32) -> SuspendedTries {
        let count = self.try_depth() - down_to_depth;
        for _ in 0..count {
            let open = self.close_try();
            self.suspended_tries.push(open);
        }
        SuspendedTries {
            count,
            at: self.len,
        }
    }

    #[expect(clippy::needless_pass_by_value, reason = "once only")]
    pub(crate) fn resume_tries(&mut self, suspended: SuspendedTries) {
        let SuspendedTries { count, at } = suspended;
        // What cannot throw may as well be inside.
        let start = if self.noticed_throw > at {
            self.len
        } else {
            at
        };
        let first = self.suspended_tries.len() - count as usize;
        let resumed = self.suspended_tries.drain(first..).rev();
        self.tries.extend(resumed.map(|open| OpenTry {
            start,
            throws: false,
            ..open
        }));
        self.note_try_boundary();
    }

    // Tables

    /// Of two cases with one key the first counts.
    pub(crate) fn add_int_switch(
        &mut self,
        default: Label,
        cases: &[(i32, Label)],
    ) -> IntSwitchIndex {
        if !self.is_reachable {
            return IntSwitchIndex(0);
        }
        let mut sorted: Vec<(i32, u32)> =
            cases.iter().map(|&(key, label)| (key, label.0)).collect();
        sorted.sort_by_key(|&(key, _)| key);
        sorted.dedup_by_key(|&mut (key, _)| key);
        for &(_, label) in &sorted {
            self.refer_to(Label(label));
        }
        let default = self.refer_to(default);
        let dense = match (sorted.first(), sorted.last()) {
            (Some(&(first, _)), Some(&(last, _))) => {
                let range = i64::from(last) - i64::from(first);
                let is_dense = range <= MAX_DENSE_RANGE
                    && range / (sorted.len() as i64) < MAX_DENSE_RANGE_PER_CASE;
                is_dense.then_some((first, range as usize + 1))
            }
            _ => None,
        };
        let cases = match dense {
            Some((first, len)) => {
                let mut targets = vec![default; len].into_boxed_slice();
                for (key, label) in sorted {
                    targets[(key - first) as usize] = label;
                }
                IntCases::Dense { first, targets }
            }
            None => IntCases::Sparse(sorted.into()),
        };
        self.int_switches.push(IntSwitch { default, cases });
        IntSwitchIndex(self.int_switches.len() as u32 - 1)
    }

    /// Counts in unreachable code too, so that the index agrees with that of the keys.
    pub(crate) fn add_string_switch(
        &mut self,
        default: Label,
        targets: &[Label],
    ) -> StringSwitchIndex {
        let mut word = |label| {
            if self.is_reachable {
                self.refer_to(label)
            } else {
                NOWHERE
            }
        };
        let switch = StringSwitch {
            default: word(default),
            targets: targets.iter().copied().map(word).collect(),
        };
        self.string_switches.push(switch);
        StringSwitchIndex(self.string_switches.len() as u32 - 1)
    }

    pub(crate) fn add_spread_mask(&mut self, bits: impl Iterator<Item = bool>) -> SpreadMaskIndex {
        self.mask.clear();
        for (index, _) in bits.enumerate().filter(|&(_, bit)| bit) {
            self.mask.resize(index / 32 + 1, 0);
            self.mask[index / 32] |= 1 << (index % 32);
        }
        self.share_mask()
    }

    fn share_mask(&mut self) -> SpreadMaskIndex {
        if !self.is_reachable {
            return SpreadMaskIndex(0);
        }
        let first = self.spread_masks.len().saturating_sub(MASKS_COMPARED);
        let same = self.spread_masks[first..]
            .iter()
            .position(|mask| **mask == *self.mask);
        if let Some(position) = same {
            return SpreadMaskIndex((first + position) as u32);
        }
        self.spread_masks.push(self.mask.as_slice().into());
        SpreadMaskIndex(self.spread_masks.len() as u32 - 1)
    }

    /// The index is only for the operands: `finish` numbers the points that are left again.
    pub(crate) fn add_suspend_point(&mut self, kind: SuspendKind) -> SuspendPointIndex {
        self.suspend_kinds.push(kind);
        SuspendPointIndex(self.suspend_kinds.len() as u32 - 1)
    }

    /// From the next instruction on.
    pub(crate) fn set_scope_register(&mut self, register: Register) {
        self.scope_registers.push((self.len, register));
    }

    pub(crate) fn add_coverage_block(&mut self, block: CoverageBlock) -> CoverageBlockIndex {
        self.coverage_blocks.push(block);
        CoverageBlockIndex(self.coverage_blocks.len() as u32 - 1)
    }
}
