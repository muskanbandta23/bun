//! What was true of cells when the graph was built, and is checked or watched.

use bun_engine_vm::{CellType, IndexingMode, ShapeId, SpeculatedType, TypeFlags};

#[derive(Clone, Copy)]
pub(crate) struct ShapeFacts {
    pub(crate) id: ShapeId,
    /// Of the shape itself, which the dependencies keep alive.
    pub(crate) address: usize,
    pub(crate) cell_type: CellType,
    pub(crate) ty: SpeculatedType,
    pub(crate) indexing_mode: IndexingMode,
    pub(crate) type_flags: TypeFlags,
    /// The first word of an instance.
    pub(crate) header: u64,
    pub(crate) inline_capacity: u32,
    pub(crate) out_of_line_capacity: u32,
    /// The bits of the value.
    pub(crate) prototype: u64,
    pub(crate) global: usize,
    pub(crate) is_original_array: bool,
    /// No instance changes to another shape without the code being invalidated.
    pub(crate) is_stable: bool,
}

/// A position in `Graph::shapes`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(crate) struct ShapeIndex(pub(crate) u32);

/// A set of shapes. Equal sets have equal numbers.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ShapeSet(pub(crate) u32);

impl ShapeSet {
    pub(crate) const ANY: Self = Self(u32::MAX);
    const FIRST_LIST: u32 = 1 << 31;

    pub(crate) fn single(shape: ShapeIndex) -> Self {
        Self(shape.0)
    }

    pub(crate) fn only(self) -> Option<ShapeIndex> {
        (self.0 < Self::FIRST_LIST).then_some(ShapeIndex(self.0))
    }
}

#[derive(Default)]
pub(crate) struct Shapes {
    facts: Vec<ShapeFacts>,
    /// `ShapeIndex(n)` at position `n`, so that a single shape is a slice too.
    identity: Vec<ShapeIndex>,
    lists: Vec<Box<[ShapeIndex]>>,
}

impl core::ops::Index<ShapeIndex> for Shapes {
    type Output = ShapeFacts;

    fn index(&self, shape: ShapeIndex) -> &ShapeFacts {
        &self.facts[shape.0 as usize]
    }
}

impl Shapes {
    const MAX_MEMBERS: usize = 8;

    pub(crate) fn add(&mut self, facts: ShapeFacts) -> ShapeIndex {
        let index = ShapeIndex(self.facts.len() as u32);
        self.facts.push(facts);
        self.identity.push(index);
        index
    }

    pub(crate) fn all(&self) -> &[ShapeFacts] {
        &self.facts
    }

    /// Ascending. Not of `ANY`.
    pub(crate) fn members(&self, set: ShapeSet) -> &[ShapeIndex] {
        match set.only() {
            Some(shape) => core::slice::from_ref(&self.identity[shape.0 as usize]),
            None => &self.lists[(set.0 - ShapeSet::FIRST_LIST) as usize],
        }
    }

    pub(crate) fn set_of(&mut self, members: impl IntoIterator<Item = ShapeIndex>) -> ShapeSet {
        let mut members: smallvec::SmallVec<[ShapeIndex; 16]> = members.into_iter().collect();
        members.sort_unstable();
        members.dedup();
        if let [only] = members[..] {
            return ShapeSet::single(only);
        }
        let position = self
            .lists
            .iter()
            .position(|list| **list == *members)
            .unwrap_or_else(|| {
                self.lists.push(members.as_slice().into());
                self.lists.len() - 1
            });
        ShapeSet(ShapeSet::FIRST_LIST + position as u32)
    }

    pub(crate) fn is_subset(&self, part: ShapeSet, whole: ShapeSet) -> bool {
        part == whole
            || whole == ShapeSet::ANY
            || part != ShapeSet::ANY
                && self
                    .members(part)
                    .iter()
                    .all(|shape| self.members(whole).contains(shape))
    }

    pub(crate) fn union(&mut self, a: ShapeSet, b: ShapeSet) -> ShapeSet {
        if self.is_subset(a, b) {
            return b;
        }
        if self.is_subset(b, a) {
            return a;
        }
        let both: smallvec::SmallVec<[ShapeIndex; 16]> = self
            .members(a)
            .iter()
            .chain(self.members(b))
            .copied()
            .collect();
        if both.len() > Self::MAX_MEMBERS {
            ShapeSet::ANY
        } else {
            self.set_of(both)
        }
    }

    pub(crate) fn intersection(&mut self, a: ShapeSet, b: ShapeSet) -> ShapeSet {
        if self.is_subset(a, b) {
            return a;
        }
        if self.is_subset(b, a) {
            return b;
        }
        let common: smallvec::SmallVec<[ShapeIndex; 16]> = self
            .members(a)
            .iter()
            .filter(|shape| self.members(b).contains(shape))
            .copied()
            .collect();
        self.set_of(common)
    }

    pub(crate) fn is_stable(&self, set: ShapeSet) -> bool {
        set != ShapeSet::ANY && self.members(set).iter().all(|&shape| self[shape].is_stable)
    }

    pub(crate) fn type_of(&self, set: ShapeSet) -> SpeculatedType {
        if set == ShapeSet::ANY {
            return SpeculatedType::CELL;
        }
        self.members(set)
            .iter()
            .fold(SpeculatedType::empty(), |ty, &shape| ty | self[shape].ty)
    }
}

/// Of a cell that is a constant.
#[derive(Clone)]
pub(crate) struct CellFacts {
    pub(crate) ty: SpeculatedType,
    /// `ANY` unless the shape is stable.
    pub(crate) shapes: ShapeSet,
    pub(crate) detail: CellDetail,
}

#[derive(Clone)]
pub(crate) enum CellDetail {
    None,
    String { length: u32 },
    Function { executable: usize },
}
