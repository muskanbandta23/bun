//! Finds the value that already computes an operation.

use core::hash::{Hash, Hasher};

use super::writes::Writes;
use crate::hash::FastHasher;
use crate::ir::{
    Access, AccessFlags, Binary, Checked, EffectFlags, Exit, Function, Heap, Load, Op, State,
    Store, Token, Type, Value,
};

#[derive(Clone, Copy)]
struct Entry {
    /// For a load it may be a store to the same bytes.
    value: Value,
    hash: u32,
    /// The position of the walk from which on the value is not available.
    end: u32,
    /// For what depends on memory, see [`Writes::now`].
    time: u32,
}

const EMPTY: Entry = Entry {
    value: Value::NONE,
    hash: 0,
    end: 0,
    time: 0,
};

/// An open hash table. The walk never comes back to a subtree, so entries expire instead of being removed.
#[derive(Default)]
pub(super) struct Numbering {
    entries: Vec<Entry>,
    occupied: usize,
    position: u32,
    end_of_subtree: u32,
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
pub(super) fn key(function: &Function, mut op: Op) -> Option<(Op, Heap)> {
    const UNREPEATABLE: EffectFlags = EffectFlags::EXITS
        .union(EffectFlags::CONTROL_DEPENDENT)
        .complement();
    let reads = match &mut op {
        Op::Checked(_, _, exit) => {
            *exit = Exit {
                token: Token(0),
                state: State::NONE,
            };
            Heap::NOTHING
        }
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

/// The value that `known` has to offer for a key.
fn offer(function: &Function, known: Value, wanted: &Op) -> Option<Value> {
    let op = *function.op(known);
    if let Some((load, _)) = load_after(op) {
        return (load == *wanted).then(|| function.operands(known)[0]);
    }
    let matches = match (key(function, op)?.0, *wanted) {
        (
            Op::WideNode {
                kind, args, data, ..
            },
            Op::WideNode {
                kind: same_kind,
                args: same_args,
                data: same_data,
                ..
            },
        ) => {
            (kind, data) == (same_kind, same_data)
                && function.values_in(args) == function.values_in(same_args)
        }
        (key, wanted) => key == wanted,
    };
    matches.then_some(known)
}

impl Numbering {
    pub(super) fn reset(&mut self) {
        const INITIAL_CAPACITY: usize = 256;
        self.entries.clear();
        self.entries.resize(INITIAL_CAPACITY, EMPTY);
        self.occupied = 0;
    }

    pub(super) fn enter(&mut self, position: u32, end_of_subtree: u32) {
        (self.position, self.end_of_subtree) = (position, end_of_subtree);
    }

    /// `accept` gives the result, or whether the entry is of no more use.
    #[inline(always)]
    fn probe(
        &self,
        hash: u32,
        mut accept: impl FnMut(&Entry) -> Result<Value, bool>,
    ) -> Result<Value, Vacancy> {
        let mask = self.entries.len() - 1;
        let (mut slot, mut free) = (hash as usize & mask, None);
        loop {
            let entry = &self.entries[slot];
            if entry.value.is_none() {
                let slot = free.unwrap_or(slot);
                return Err(Vacancy { slot, hash });
            }
            if entry.end <= self.position {
                free.get_or_insert(slot);
            } else if entry.hash == hash {
                match accept(entry) {
                    Ok(value) => return Ok(value),
                    Err(true) => free = Some(slot),
                    Err(false) => {}
                }
            }
            slot = (slot + 1) & mask;
        }
    }

    #[inline]
    pub(super) fn find_expression(
        &self,
        function: &Function,
        wanted: &Op,
        ty: Type,
    ) -> Result<Value, Vacancy> {
        let mut hasher = FastHasher::default();
        wanted.hash(&mut hasher);
        self.probe(hasher.finish() as u32 ^ ty as u32, |entry| {
            let known = function.op(entry.value);
            let matches = known == wanted
                || matches!((*known, *wanted), (Op::Checked(checked, args, _), Op::Binary(op, same))
                    if unchecked(checked) == op && args == same);
            let matches = matches && function.type_of(entry.value) == ty;
            matches.then_some(entry.value).ok_or(false)
        })
    }

    pub(super) fn find(
        &self,
        function: &Function,
        writes: &mut Writes,
        (wanted, reads): (Op, Heap),
        ty: Type,
    ) -> Result<Value, Vacancy> {
        /// Outdated entries stay for the sake of other subtrees, where they may still hold.
        const MOST_OUTDATED: u32 = 3;
        let mut hasher = FastHasher::default();
        match wanted {
            Op::WideNode {
                kind, args, data, ..
            } => {
                (kind, data, function.values_in(args)).hash(&mut hasher);
            }
            _ => wanted.hash(&mut hasher),
        }
        let mut outdated = 0;
        self.probe(hasher.finish() as u32 ^ ty as u32, |entry| {
            let offered = offer(function, entry.value, &wanted);
            let offered = offered
                .filter(|&value| function.type_of(value) == ty)
                .ok_or(false)?;
            let access = function.op(entry.value).access();
            if access.is_some_and(|access| access.has(AccessFlags::IMMUTABLE))
                || !writes.may_have_changed(function, reads, entry.time)
            {
                return Ok(offered);
            }
            outdated += 1;
            Err(entry.end == self.end_of_subtree || outdated == MOST_OUTDATED)
        })
    }

    /// Nothing has been added since the vacancy was found.
    #[inline]
    pub(super) fn add(&mut self, vacancy: &Vacancy, value: Value, time: u32) {
        let entry = &mut self.entries[vacancy.slot];
        self.occupied += usize::from(entry.value.is_none());
        *entry = Entry {
            value,
            hash: vacancy.hash,
            end: self.end_of_subtree,
            time,
        };
        if self.occupied * 2 > self.entries.len() {
            self.rebuild();
        }
    }

    #[cold]
    fn rebuild(&mut self) {
        let position = self.position;
        let is_current = |entry: &Entry| entry.end > position;
        self.occupied = self
            .entries
            .iter()
            .filter(|&entry| is_current(entry))
            .count();
        let grows = self.occupied * 4 > self.entries.len();
        let capacity = self.entries.len() << usize::from(grows);
        let old = core::mem::replace(&mut self.entries, vec![EMPTY; capacity]);
        for entry in old.into_iter().filter(is_current) {
            let mut slot = entry.hash as usize & (capacity - 1);
            while !self.entries[slot].value.is_none() {
                slot = (slot + 1) & (capacity - 1);
            }
            self.entries[slot] = entry;
        }
    }
}
