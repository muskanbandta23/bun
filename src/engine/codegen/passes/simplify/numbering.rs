//! Finds the value that already computes an operation.

use core::hash::{Hash, Hasher};

use super::{Simplifier, Tables};
use crate::ir::{
    Access, AccessFlags, Binary, Checked, EffectFlags, Function, Heap, Load, Op, State, Store,
    Type, Value,
};

#[derive(Clone, Copy)]
struct Entry {
    /// For a load it may be a store to the same bytes.
    value: Value,
    hash: u32,
}

const EMPTY: Entry = Entry {
    value: Value::NONE,
    hash: 0,
};

/// An open hash table. The walk never comes back to a subtree, so what is defined there can be overwritten.
#[derive(Default)]
pub(super) struct Numbering {
    entries: Vec<Entry>,
    occupied: usize,
}

/// Mixes at the end only.
#[derive(Default)]
struct Words(u64);

impl Words {
    #[inline(always)]
    fn add(&mut self, word: u64) {
        self.0 = self.0.rotate_left(21).wrapping_add(word);
    }

    #[inline(always)]
    fn with_type(mut self, ty: Type) -> u32 {
        self.add(ty as u64);
        self.finish() as u32
    }
}

impl Hasher for Words {
    #[inline(always)]
    fn finish(&self) -> u64 {
        (self.0 ^ self.0 >> 29).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 32
    }

    fn write(&mut self, bytes: &[u8]) {
        bytes.iter().for_each(|&byte| self.add(byte.into()));
    }

    #[inline(always)]
    fn write_u8(&mut self, value: u8) {
        self.add(value.into());
    }

    #[inline(always)]
    fn write_u16(&mut self, value: u16) {
        self.add(value.into());
    }

    #[inline(always)]
    fn write_u32(&mut self, value: u32) {
        self.add(value.into());
    }

    #[inline(always)]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }
}

/// Where an entry can go.
pub(super) struct Vacancy {
    slot: usize,
    hash: u32,
}

pub(super) fn unchecked(op: Checked) -> Binary {
    match op {
        Checked::Add => Binary::Add,
        Checked::Sub => Binary::Sub,
        Checked::Mul => Binary::Mul,
    }
}

/// The value depends on the operands alone.
#[inline]
pub(super) fn is_expression(op: &Op) -> bool {
    matches!(
        op,
        Op::Unary(..)
            | Op::Binary(..)
            | Op::Compare(..)
            | Op::FloatCompare(..)
            | Op::Select(_)
            | Op::Wide(..)
            | Op::MulWide(..)
            | Op::Extract(..)
            | Op::VectorUnary(..)
            | Op::VectorBinary(..)
            | Op::VectorTernary(..)
            | Op::VectorShift(..)
            | Op::VectorReduce(..)
            | Op::Shuffle(..)
            | Op::Splat(..)
            | Op::ExtractLane { .. }
            | Op::ReplaceLane { .. }
    )
}

/// For what is no expression: the operation without what does not decide its outcome, and the heap that does.
#[inline(always)]
pub(super) fn key(function: &Function, mut op: Op) -> Option<(Op, Heap)> {
    const UNREPEATABLE: EffectFlags = EffectFlags::EXITS
        .union(EffectFlags::CONTROL_DEPENDENT)
        .complement();
    let reads = match &mut op {
        Op::Load { access, .. } | Op::LoadLane { access, .. }
            if !access.has(AccessFlags::ATOMIC) =>
        {
            core::mem::replace(access, Heap::NOTHING.into()).heap()
        }
        Op::Node { kind, state, .. } | Op::WideNode { kind, state, .. } => {
            let effects = function.node_info(*kind).effects;
            if effects.writes_memory() || effects.has(UNREPEATABLE) {
                return None;
            }
            *state = State::NONE;
            effects.reads
        }
        _ => return None,
    };
    Some((op, reads))
}

/// The key of the load that finds what a store has left.
#[inline(always)]
pub(super) fn load_after(store: Op) -> Option<(Op, Heap)> {
    match store {
        Op::Store {
            kind: Store::Full,
            args: [_, address],
            offset,
            access,
        } if !access.has(AccessFlags::ATOMIC) => {
            let load = Op::Load {
                kind: Load::Full,
                address,
                offset,
                access: Access::from(Heap::NOTHING),
            };
            Some((load, access.heap()))
        }
        _ => None,
    }
}

/// The value that `known` has to offer for a key, or else whether it is gone.
fn offer(function: &Function, known: Value, wanted: &Op, ty: Type) -> Result<Value, bool> {
    let op = *function.op(known);
    let offered = match (load_after(op), key(function, op), *wanted) {
        (Some((load, _)), ..) => (load == *wanted).then(|| function.operands(known)[0]),
        (
            _,
            Some((
                Op::WideNode {
                    kind, args, data, ..
                },
                _,
            )),
            Op::WideNode {
                kind: same_kind,
                args: same_args,
                data: same_data,
                ..
            },
        ) => {
            let matches = (kind, data) == (same_kind, same_data)
                && function.values_in(args) == function.values_in(same_args);
            matches.then_some(known)
        }
        (_, Some((key, _)), wanted) => (key == wanted).then_some(known),
        _ => return Err(matches!(op, Op::Removed | Op::Alias(_))),
    };
    offered
        .filter(|&value| function.type_of(value) == ty)
        .ok_or(false)
}

impl Numbering {
    pub(super) fn reset(&mut self, expected: usize) {
        self.entries.clear();
        self.entries
            .resize((2 * expected + 16).next_power_of_two(), EMPTY);
        self.occupied = 0;
    }

    /// `accept` gives the result, or whether the entry is of no more use.
    #[inline(always)]
    fn probe(
        &self,
        hash: u32,
        mut accept: impl FnMut(Value) -> Result<Value, bool>,
    ) -> Result<Value, Vacancy> {
        let mask = self.entries.len() - 1;
        let (mut slot, mut free) = (hash as usize & mask, None);
        loop {
            let entry = self.entries[slot];
            if entry.value.is_none() {
                let slot = free.unwrap_or(slot);
                return Err(Vacancy { slot, hash });
            }
            if entry.hash == hash {
                match accept(entry.value) {
                    Ok(value) => return Ok(value),
                    Err(true) => free = Some(slot),
                    Err(false) => {}
                }
            }
            slot = (slot + 1) & mask;
        }
    }

    /// Nothing has been added since the vacancy was found.
    #[inline]
    pub(super) fn add(&mut self, vacancy: &Vacancy, value: Value) {
        let (entry, hash) = (&mut self.entries[vacancy.slot], vacancy.hash);
        self.occupied += usize::from(entry.value.is_none());
        *entry = Entry { value, hash };
        if self.occupied * 3 > self.entries.len() * 2 {
            self.grow();
        }
    }

    #[cold]
    fn grow(&mut self) {
        let capacity = self.entries.len() * 2;
        let old = core::mem::replace(&mut self.entries, vec![EMPTY; capacity]);
        for entry in old.into_iter().filter(|entry| !entry.value.is_none()) {
            let mut slot = entry.hash as usize & (capacity - 1);
            while !self.entries[slot].value.is_none() {
                slot = (slot + 1) & (capacity - 1);
            }
            self.entries[slot] = entry;
        }
    }
}

impl Simplifier<'_> {
    /// The position of the walk from which on the value is not available.
    #[inline]
    fn end_of_scope(&self, value: Value) -> u32 {
        self.dominators
            .end_of_subtree(self.function.block_of(value))
    }

    /// Also for arithmetic that exits on overflow, which can stand in for the usual kind.
    #[inline(always)]
    pub(super) fn find_expression(&self, wanted: &Op, ty: Type) -> Result<Value, Vacancy> {
        let mut words = Words::default();
        match *wanted {
            Op::Binary(op, [a, b]) => (op, a, b).hash(&mut words),
            Op::Checked(op, [a, b], _) => (unchecked(op), a, b).hash(&mut words),
            Op::Compare(cond, [a, b]) => (a, b, cond).hash(&mut words),
            Op::Unary(op, a) => (op, a).hash(&mut words),
            _ => wanted.hash(&mut words),
        }
        self.tables.numbering.probe(words.with_type(ty), |known| {
            self.accept_expression(known, wanted, ty)
        })
    }

    fn accept_expression(&self, known: Value, wanted: &Op, ty: Type) -> Result<Value, bool> {
        if self.end_of_scope(known) <= self.position {
            return Err(true);
        }
        let matches = match (self.op(known), *wanted) {
            (Op::Checked(op, args, _), Op::Checked(same, others, _)) => {
                (op, args) == (same, others)
            }
            (Op::Checked(op, args, _), wanted) => Op::Binary(unchecked(op), args) == wanted,
            (Op::Removed | Op::Alias(_), _) => return Err(true),
            (op, wanted) => op == wanted,
        };
        (matches && self.type_of(known) == ty)
            .then_some(known)
            .ok_or(false)
    }

    #[inline(always)]
    pub(super) fn find_by_key(
        &mut self,
        (wanted, reads): (Op, Heap),
        ty: Type,
    ) -> Result<Value, Vacancy> {
        /// Outdated entries stay for the sake of other subtrees, where they may still hold.
        const MOST_OUTDATED: u32 = 3;
        let mut words = Words::default();
        match wanted {
            Op::Load {
                kind,
                address,
                offset,
                ..
            } => (kind, address, offset).hash(&mut words),
            Op::WideNode {
                kind, args, data, ..
            } => {
                (kind, data).hash(&mut words);
                for arg in self.function.values_in(args) {
                    words.0 = words.finish();
                    arg.hash(&mut words);
                }
            }
            _ => wanted.hash(&mut words),
        }
        let mut outdated = 0;
        let Tables {
            numbering,
            writes,
            values,
            ..
        } = &mut self.tables;
        numbering.probe(words.with_type(ty), |known| {
            let end = self
                .dominators
                .end_of_subtree(self.function.block_of(known));
            if end <= self.position {
                return Err(true);
            }
            let offered = offer(self.function, known, &wanted, ty)?;
            let access = self.function.op(known).access();
            if access.is_some_and(|access| access.has(AccessFlags::IMMUTABLE))
                || !writes.may_have_changed(self.function, reads, values[known].mark)
            {
                return Ok(offered);
            }
            outdated += 1;
            Err(end == self.end_of_subtree || outdated == MOST_OUTDATED)
        })
    }

    #[inline]
    pub(super) fn add_with_time(&mut self, vacancy: &Vacancy, value: Value) {
        self.tables.values[value].mark = self.tables.writes.now();
        self.tables.numbering.add(vacancy, value);
    }
}
