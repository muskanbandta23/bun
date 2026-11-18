//! The bytes, labels and unresolved references of code that is being assembled.

use crate::label::{CodeTooLarge, Label, LabelState, Target, resolve};

/// Longer than any instruction plus the widest single write.
pub(crate) const WINDOW: usize = 32;

const INITIAL_CAPACITY: usize = 512;
const MINIMUM_ALIGNMENT: u32 = 16;

/// Bytes that depend on where a label ends up. `Kind` is the architecture's.
#[derive(Clone, Copy)]
pub(crate) struct Site<Kind> {
    pub(crate) at: u32,
    pub(crate) label: Label,
    pub(crate) kind: Kind,
}

#[derive(Clone, Copy)]
pub(crate) struct Constant {
    pub(crate) bits: u128,
    pub(crate) size: u8,
    pub(crate) label: Label,
}

pub(crate) struct JumpTable {
    pub(crate) label: Label,
    pub(crate) targets_end: u32,
}

pub(crate) struct CodeBuffer<Kind> {
    /// Initialized beyond `len`, so that an instruction is written with one check of the bounds.
    pub(crate) bytes: Vec<u8>,
    pub(crate) len: usize,
    pub(crate) labels: Vec<LabelState>,
    pub(crate) sites: Vec<Site<Kind>>,
    pub(crate) externals: Vec<Target>,
    pub(crate) constants: Vec<Constant>,
    pub(crate) tables: Vec<JumpTable>,
    pub(crate) table_targets: Vec<Label>,
    pub(crate) alignment: u32,
    /// Up to here a jump may be written over the code, so control does not enter.
    pub(crate) replaceable_end: usize,
    pub(crate) limit: usize,
    pub(crate) overflowed: bool,
}

impl<Kind: Copy> CodeBuffer<Kind> {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            bytes: vec![0; INITIAL_CAPACITY],
            len: 0,
            labels: Vec::new(),
            sites: Vec::new(),
            externals: Vec::new(),
            constants: Vec::new(),
            tables: Vec::new(),
            table_targets: Vec::new(),
            alignment: MINIMUM_ALIGNMENT,
            replaceable_end: 0,
            limit,
            overflowed: false,
        }
    }

    pub(crate) fn reserve(&mut self, additional: usize) {
        let wanted = (self.len + additional.min(self.limit)).min(self.limit) + WINDOW;
        if wanted > self.bytes.len() {
            self.bytes.resize(wanted, 0);
        }
    }

    #[inline(always)]
    pub(crate) fn window(&mut self) -> &mut [u8; WINDOW] {
        if self.len + WINDOW > self.bytes.len() {
            self.grow();
        }
        self.bytes[self.len..].first_chunk_mut().unwrap()
    }

    /// Past the limit the code is lost, so what follows is written over it and `finish` fails.
    #[cold]
    #[inline(never)]
    fn grow(&mut self) {
        if self.len > self.limit {
            self.overflowed = true;
            self.len = 0;
            self.replaceable_end = 0;
            return;
        }
        let capacity = (self.bytes.len() * 2).min(self.limit + 2 * WINDOW);
        self.bytes.resize(capacity, 0);
    }

    pub(crate) fn check_size(&self) -> Result<(), CodeTooLarge> {
        if self.overflowed || self.len > self.limit {
            Err(CodeTooLarge)
        } else {
            Ok(())
        }
    }

    #[inline(always)]
    pub(crate) fn put<const N: usize>(&mut self, bytes: [u8; N]) {
        *self.window().first_chunk_mut().unwrap() = bytes;
        self.len += N;
    }

    #[inline]
    pub(crate) fn offset(&self) -> u32 {
        self.len as u32
    }

    #[inline]
    fn push_label(&mut self, offset: u32, link: u32) -> Label {
        let label = Label(self.labels.len() as u32);
        self.labels.push(LabelState { offset, link });
        label
    }

    #[inline]
    pub(crate) fn new_label(&mut self) -> Label {
        self.push_label(LabelState::UNBOUND, 0)
    }

    #[inline]
    pub(crate) fn mark(&mut self) -> Label {
        self.push_label(self.len as u32, self.sites.len() as u32)
    }

    #[inline]
    pub(crate) fn external(&mut self, target: Target) -> Label {
        self.externals.push(target);
        self.push_label(LabelState::EXTERNAL, self.externals.len() as u32 - 1)
    }

    #[inline]
    pub(crate) fn bind(&mut self, label: Label) {
        let state = &mut self.labels[label.0 as usize];
        assert!(
            state.offset == LabelState::UNBOUND,
            "the label is bound twice"
        );
        *state = LabelState {
            offset: self.len as u32,
            link: self.sites.len() as u32,
        };
    }

    pub(crate) fn alias(&mut self, label: Label, mut target: Label) {
        while self.labels[target.0 as usize].offset == LabelState::ALIAS {
            target = Label(self.labels[target.0 as usize].link);
        }
        assert!(label != target, "the label stands for itself");
        let state = &mut self.labels[label.0 as usize];
        assert!(
            state.offset == LabelState::UNBOUND,
            "the label is bound twice"
        );
        *state = LabelState {
            offset: LabelState::ALIAS,
            link: target.0,
        };
    }

    #[inline]
    pub(crate) fn state(&self, label: Label) -> LabelState {
        resolve(&self.labels, label)
    }

    #[inline]
    pub(crate) fn record(&mut self, at: u32, label: Label, kind: Kind) {
        self.sites.push(Site { at, label, kind });
    }

    pub(crate) fn require_alignment(&mut self, alignment: u32) {
        assert!(alignment.is_power_of_two());
        self.alignment = self.alignment.max(alignment);
    }

    /// The label of the low `size` bytes of `bits`, which are placed after the instructions.
    pub(crate) fn constant(&mut self, bits: u128, size: u8) -> Label {
        const RECENT: usize = 16;
        let recent = &self.constants[self.constants.len().saturating_sub(RECENT)..];
        if let Some(found) = recent
            .iter()
            .find(|constant| constant.bits == bits && constant.size == size)
        {
            return found.label;
        }
        let label = self.new_label();
        self.constants.push(Constant { bits, size, label });
        label
    }

    pub(crate) fn jump_table(&mut self, targets: &[Label]) -> Label {
        let label = self.new_label();
        self.table_targets.extend_from_slice(targets);
        self.tables.push(JumpTable {
            label,
            targets_end: self.table_targets.len() as u32,
        });
        label
    }

    /// Follows the instructions, at a multiple of 16. `entry` is the kind of site for a word of the table at a label.
    pub(crate) fn put_data(&mut self, entry: impl Fn(Label) -> Kind) {
        let constants = core::mem::take(&mut self.constants);
        for size in [16, 8, 4] {
            for constant in constants.iter().filter(|constant| constant.size == size) {
                self.bind(constant.label);
                self.put(constant.bits.to_le_bytes());
                self.len -= 16 - size as usize;
            }
        }
        let mut first = 0;
        for table in core::mem::take(&mut self.tables) {
            self.bind(table.label);
            for index in first..table.targets_end as usize {
                self.record(self.offset(), self.table_targets[index], entry(table.label));
                self.put([0; 4]);
            }
            first = table.targets_end as usize;
        }
    }

    pub(crate) fn has_data(&self) -> bool {
        !self.constants.is_empty() || !self.tables.is_empty()
    }
}
