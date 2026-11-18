//! What a pattern is compiled to, for the interpreter and for the generator of machine code.

mod op;
mod scan;
mod verify;

use core::ops::Range;

use bun_engine_unicode::IgnoreCase;

pub(crate) use op::split_halves;
pub use op::{
    ClassId, Comparison, Direction, Filter, LoopId, Op, Pairs, Place, ScanId, ScopeId, Slot, Stride,
    SwitchId, Target, UNBOUNDED, UNSET, join_halves,
};
pub use scan::{Probe, RowProbe, Scan};
pub use verify::Verified;

use crate::set::{LookupTable, Set};

/// How the subject stores its characters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Width {
    Latin1,
    Utf16,
}

impl Width {
    #[inline]
    pub fn bytes_per_unit(self) -> u32 {
        match self {
            Width::Latin1 => 1,
            Width::Utf16 => 2,
        }
    }

    #[inline]
    pub fn max_unit(self) -> u16 {
        match self {
            Width::Latin1 => 0xff,
            Width::Utf16 => 0xffff,
        }
    }
}

pub(crate) const MAX_LENGTH: u32 = bun_engine_text::MAX_LENGTH as u32;

#[inline]
fn add_lengths(a: u32, b: u32) -> u32 {
    a.saturating_add(b).min(MAX_LENGTH)
}

/// How many units are known to exist on either side of a position.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Room {
    pub(crate) ahead: u32,
    pub(crate) behind: u32,
}

impl Room {
    pub(crate) const NONE: Room = Room {
        ahead: 0,
        behind: 0,
    };

    #[inline]
    pub(crate) fn toward(self, direction: Direction) -> u32 {
        match direction {
            Direction::Forward => self.ahead,
            Direction::Backward => self.behind,
        }
    }

    #[inline]
    pub(crate) fn with(self, direction: Direction, units: u32) -> Room {
        match direction {
            Direction::Forward => Room {
                ahead: units,
                ..self
            },
            Direction::Backward => Room {
                behind: units,
                ..self
            },
        }
    }

    #[inline]
    pub(crate) fn covers(self, other: Room) -> bool {
        self.ahead >= other.ahead && self.behind >= other.behind
    }

    #[inline]
    pub(crate) fn min(self, other: Room) -> Room {
        Room {
            ahead: self.ahead.min(other.ahead),
            behind: self.behind.min(other.behind),
        }
    }

    /// As seen from `delta` units further, if that is known to be in the subject.
    #[inline]
    pub(crate) fn moved(self, delta: i32) -> Option<Room> {
        let distance = delta.unsigned_abs();
        Some(if delta >= 0 {
            Room {
                ahead: self.ahead.checked_sub(distance)?,
                behind: add_lengths(self.behind, distance),
            }
        } else {
            Room {
                ahead: add_lengths(self.ahead, distance),
                behind: self.behind.checked_sub(distance)?,
            }
        })
    }

    /// Of a position that does not compare so with what is `distance` units from one that has the room `other`.
    pub(crate) fn unless(self, when: Comparison, other: Room, distance: i64) -> Room {
        let clamp = |units: i64| units.clamp(0, MAX_LENGTH.into()) as u32;
        match when {
            Comparison::Below => Room {
                behind: self.behind.max(clamp(i64::from(other.behind) + distance)),
                ..self
            },
            Comparison::Above => Room {
                ahead: self.ahead.max(clamp(i64::from(other.ahead) - distance)),
                ..self
            },
            Comparison::Equal | Comparison::AtLeast | Comparison::AtMost => self,
        }
    }

    /// Whether `count` units from `offset` up exist.
    #[inline]
    pub(crate) fn has(self, offset: i32, count: u32) -> bool {
        let offset = i64::from(offset);
        -offset <= i64::from(self.behind) && offset + i64::from(count) <= i64::from(self.ahead)
    }
}

/// An operation that others go to, and what holds there.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Join {
    pub(crate) at: Target,
    pub(crate) room: Room,
    pub(crate) scope: ScopeId,
}

/// A node of the tree that says what the cells and the stack hold. What the ancestors of the current scope say holds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Scope {
    pub(crate) parent: ScopeId,
    /// The number of cells that are taken. Those of the scope itself start at the height of its parent.
    pub(crate) height: u32,
    pub(crate) kind: ScopeKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ScopeKind {
    Root,
    Position(Room),
    /// Any bits.
    Numbers,
    /// The height of the stack.
    Mark,
    /// Keeps a cell for a child of the kind `Choice`.
    Reserved,
    /// The cell of the parent selects a target. The cells above it hold what the scope of that target says.
    Choice {
        first_target: u32,
        end_target: u32,
        /// The stack can be higher than in the parent.
        has_frames: bool,
    },
    /// The number of frames that the loop has on the stack.
    Turns(LoopId),
    /// The last frame of the loop is on top of the stack.
    InTurn(LoopId),
    /// If there has been a turn, the cells and the stack are as the place to go back into it needs them.
    BetweenTurns(LoopId),
}

/// A loop whose turns can be gone back into.
#[derive(Clone, Copy, Debug)]
pub struct Loop {
    pub(crate) turns: Slot,
    pub(crate) in_turn: ScopeId,
    pub(crate) between_turns: ScopeId,
    pub(crate) end_cell: u32,
    pub(crate) pairs: Pairs,
    /// Around the position in a frame.
    pub(crate) room: Room,
    pub(crate) back_into_turn: Target,
}

impl Loop {
    #[inline]
    pub fn turns_cell(&self) -> u32 {
        self.turns.cell
    }

    /// Those in a frame.
    #[inline]
    pub fn cells(&self) -> Range<u32> {
        self.turns.cell + 1..self.end_cell
    }

    /// Those in a frame.
    #[inline]
    pub fn pairs(&self) -> Pairs {
        self.pairs
    }

    #[inline]
    pub fn back_into_turn(&self) -> Target {
        self.back_into_turn
    }

    /// In words of 32 bits.
    #[inline]
    pub fn frame_size(&self) -> usize {
        1 + self.cells().len() + self.pairs.entries().len()
    }

    /// Whether the frame of the first turn has the position only. Before it the cells are unused and the captures unset.
    #[inline]
    pub fn first_frame_is_short(&self) -> bool {
        self.frame_size() > 8
    }
}

/// The units from `first` up to the first of the next case go to `target`.
#[derive(Clone, Copy, Debug)]
pub struct Case {
    pub first: u16,
    pub target: Target,
}

pub struct Switch {
    /// Ascending, from zero.
    pub(crate) cases: Vec<Case>,
    /// The case of each Latin-1 character.
    pub(crate) latin1: [u8; 256],
}

impl Switch {
    #[inline]
    pub fn cases(&self) -> &[Case] {
        &self.cases
    }

    #[inline]
    pub(crate) fn target(&self, unit: u16) -> Target {
        let case = match u8::try_from(unit) {
            Ok(unit) => usize::from(self.latin1[usize::from(unit)]),
            Err(_) => self.cases.partition_point(|case| case.first <= unit) - 1,
        };
        self.cases[case].target
    }
}

pub struct Class {
    set: Set,
    table: Option<&'static LookupTable>,
}

impl Class {
    pub(crate) fn new(set: Set) -> Class {
        const RANGES_SEARCHED: usize = 8;
        let shared = set.shared();
        let large = shared.filter(|_| set.above().len() > RANGES_SEARCHED);
        Class {
            table: large.map(|shared| shared.table()),
            set,
        }
    }

    #[inline]
    pub fn set(&self) -> &Set {
        &self.set
    }

    #[inline]
    pub(crate) fn contains(&self, c: u32) -> bool {
        match (u8::try_from(c), self.table) {
            (Ok(c), _) => self.set.latin1().contains(c),
            (_, Some(table)) => table.contains(c),
            (_, None) => self.set.contains(c),
        }
    }
}

pub struct Program {
    pub(crate) ops: Vec<Op>,
    /// Ascending.
    pub(crate) joins: Vec<Join>,
    /// In preorder.
    pub(crate) scopes: Vec<Scope>,
    pub(crate) loops: Vec<Loop>,
    pub(crate) choice_targets: Vec<Target>,
    pub(crate) switches: Vec<Switch>,
    pub(crate) scans: Vec<Scan>,
    pub(crate) classes: Vec<Class>,
    pub(crate) referenced_pairs: Vec<u16>,
    /// One for each cell: how much it gains from a register.
    pub(crate) cell_weights: Vec<u32>,
    pub(crate) pair_count: u16,
    pub(crate) reports_captures: bool,
    pub(crate) width: Width,
    pub(crate) is_unicode_aware: bool,
    pub(crate) min_length: u32,
}

impl Program {
    #[inline]
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// The operations that are gone to from elsewhere, ascending.
    pub fn targets(&self) -> impl ExactSizeIterator<Item = Target> {
        self.joins.iter().map(|join| join.at)
    }

    #[inline]
    pub fn class(&self, id: ClassId) -> &Class {
        &self.classes[id.index()]
    }

    #[inline]
    pub fn class_count(&self) -> usize {
        self.classes.len()
    }

    #[inline]
    pub fn loops(&self) -> &[Loop] {
        &self.loops
    }

    #[inline]
    pub fn switch(&self, id: SwitchId) -> &Switch {
        &self.switches[id.index()]
    }

    #[inline]
    pub fn scan(&self, id: ScanId) -> &Scan {
        &self.scans[id.index()]
    }

    /// Where [`Op::Resume`] can go with this slot.
    pub fn choice_targets(&self, slot: Slot) -> &[Target] {
        match self.scopes[slot.scope.index()].kind {
            ScopeKind::Choice {
                first_target,
                end_target,
                ..
            } => &self.choice_targets[first_target as usize..end_target as usize],
            _ => &[],
        }
    }

    /// The list of [`Op::BackReference`].
    #[inline]
    pub fn referenced_pairs(&self, first: u32, count: u32) -> &[u16] {
        &self.referenced_pairs[first as usize..][..count as usize]
    }

    #[inline]
    pub fn cell_weights(&self) -> &[u32] {
        &self.cell_weights
    }

    #[inline]
    pub fn cell_count(&self) -> usize {
        self.cell_weights.len()
    }

    /// The number of starts and ends of captures.
    #[inline]
    pub fn capture_entries(&self) -> usize {
        usize::from(self.pair_count) * 2
    }

    /// Whether the caller gives the memory for the captures. Otherwise only back references read them.
    #[inline]
    pub fn reports_captures(&self) -> bool {
        self.reports_captures
    }

    #[inline]
    pub fn width(&self) -> Width {
        self.width
    }

    /// Whether a surrogate pair in the subject is one character.
    #[inline]
    pub fn reads_pairs(&self) -> bool {
        self.is_unicode_aware && self.width == Width::Utf16
    }

    /// For the back references that ignore case.
    #[inline]
    pub fn case_equivalence(&self) -> IgnoreCase {
        match self.is_unicode_aware {
            true => IgnoreCase::CodePoints,
            false => IgnoreCase::CodeUnits,
        }
    }

    /// No match is shorter.
    #[inline]
    pub fn min_length(&self) -> u32 {
        self.min_length
    }

    /// The bytes of memory that it holds.
    pub fn size(&self) -> usize {
        fn bytes<T>(elements: &Vec<T>) -> usize {
            elements.capacity() * size_of::<T>()
        }
        let cases = self.switches.iter().map(|switch| bytes(&switch.cases));
        let ranges = self.classes.iter().map(|class| class.set.owned_ranges());
        size_of::<Program>()
            + bytes(&self.ops)
            + bytes(&self.joins)
            + bytes(&self.scopes)
            + bytes(&self.loops)
            + bytes(&self.choice_targets)
            + bytes(&self.switches)
            + bytes(&self.scans)
            + self.scans.iter().map(|it| bytes(&it.rows)).sum::<usize>()
            + bytes(&self.classes)
            + bytes(&self.referenced_pairs)
            + bytes(&self.cell_weights)
            + cases.sum::<usize>()
            + ranges.sum::<usize>() * size_of::<crate::set::CharRange>()
    }
}
