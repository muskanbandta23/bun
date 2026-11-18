//! Finds the value that already computes an operation.

use core::hash::{Hash, Hasher};

use crate::hash::FastHasher;
use crate::ir::{
    Access, AccessFlags, Binary, Checked, Exit, Function, Heap, Op, State, Token, Type, Value,
};

#[derive(Clone, Copy)]
struct Entry {
    value: Value,
    hash: u32,
    /// The position of the walk from which on the value is not available.
    end: u32,
}

const EMPTY: Entry = Entry {
    value: Value::NONE,
    hash: 0,
    end: 0,
};

/// An open hash table. The walk never comes back to a subtree, so entries expire instead of being removed.
#[derive(Default)]
pub(super) struct Numbering {
    entries: Vec<Entry>,
    occupied: usize,
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

/// Without what does not decide the value. `None` if equal operations need not give equal values.
#[inline]
pub(super) fn key(mut op: Op) -> Option<Op> {
    const NO_EXIT: Exit = Exit {
        token: Token(0),
        state: State::NONE,
    };
    match &mut op {
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
        | Op::ReplaceLane { .. } => {}
        Op::Checked(_, _, exit) => *exit = NO_EXIT,
        Op::Load { access, .. } | Op::LoadLane { access, .. }
            if access.has(AccessFlags::IMMUTABLE) && !access.has(AccessFlags::ATOMIC) =>
        {
            *access = Access::new(Heap::NOTHING, AccessFlags::IMMUTABLE);
        }
        _ => return None,
    }
    Some(op)
}

#[inline]
fn computes(known: Op, wanted: Op) -> bool {
    known == wanted
        || match (known, wanted) {
            (Op::Checked(checked, args, _), Op::Binary(op, other)) => {
                unchecked(checked) == op && args == other
            }
            _ => key(known) == Some(wanted),
        }
}

impl Numbering {
    pub(super) fn reset(&mut self) {
        const INITIAL_CAPACITY: usize = 256;
        self.entries.clear();
        self.entries.resize(INITIAL_CAPACITY, EMPTY);
        self.occupied = 0;
    }

    #[inline]
    pub(super) fn find(
        &self,
        function: &Function,
        key: Op,
        ty: Type,
        position: u32,
    ) -> Result<Value, Vacancy> {
        let mut hasher = FastHasher::default();
        key.hash(&mut hasher);
        let hash = hasher.finish() as u32 ^ ty as u32;
        let mask = self.entries.len() - 1;
        let (mut slot, mut expired) = (hash as usize & mask, None);
        loop {
            let entry = self.entries[slot];
            if entry.value.is_none() {
                let slot = expired.unwrap_or(slot);
                return Err(Vacancy { slot, hash });
            }
            if entry.end <= position {
                expired.get_or_insert(slot);
            } else if entry.hash == hash
                && computes(*function.op(entry.value), key)
                && function.type_of(entry.value) == ty
            {
                return Ok(entry.value);
            }
            slot = (slot + 1) & mask;
        }
    }

    /// Nothing has been added since `find`.
    #[inline]
    pub(super) fn add(&mut self, vacancy: &Vacancy, value: Value, position: u32, end: u32) {
        let (entry, hash) = (&mut self.entries[vacancy.slot], vacancy.hash);
        self.occupied += usize::from(entry.value.is_none());
        *entry = Entry { value, hash, end };
        if self.occupied * 2 > self.entries.len() {
            self.rebuild(position);
        }
    }

    #[cold]
    fn rebuild(&mut self, position: u32) {
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
