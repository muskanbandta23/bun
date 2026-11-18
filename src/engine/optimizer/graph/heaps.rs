//! The parts of memory that instructions are told apart by.

use bun_engine_codegen::ir::{Entity, Function, Heap, HeapRange};

macro_rules! heaps {
    ($($(#[$meta:meta])* $name:ident,)*) => {
        #[allow(non_camel_case_types)]
        enum Position {
            $($name,)*
            END,
        }

        $($(#[$meta])* pub(crate) const $name: Heap = Heap::at(FIRST_LEAF + Position::$name as u32);)*
    };
}

/// All that a script can change.
pub(crate) const WORLD: Heap = Heap::at(2);
/// The shape, the indexing mode, the storage and all that is in it.
pub(crate) const OBJECTS: Heap = Heap::at(3);
/// The lengths and the elements of every kind.
pub(crate) const INDEXED: Heap = Heap::at(4);
pub(crate) const NAMED: Heap = Heap::at(5);
pub(crate) const SCOPE_SLOTS: Heap = Heap::at(6);
pub(crate) const GLOBAL_BINDINGS: Heap = Heap::at(7);
/// What changes when an object gets other kinds of elements or more slots.
pub(crate) const LAYOUT: Heap = Heap::at(8);
const FIRST_LEAF: u32 = 9;

heaps! {
    /// The header and arguments of the frame.
    FRAME,
    /// Counters and the like, which no script sees.
    SIDE_STATE,
    /// From here on it is `WORLD`.
    STRING_FIBER,
    TYPED_ARRAY_FIELDS,
    TYPED_ARRAY_DATA,
    INTERNAL_FIELDS,
    DIRECT_ARGUMENTS,
    FUNCTION_TARGET,
    REGEXP_LAST_INDEX,
    REGEXP_STATE,
    MAP_AND_SET,
    WEAK_MAP_AND_SET,
    DATE_FIELDS,
    PROMISE_FIELDS,
    RANDOM_STATE,
    WATCHPOINT_STATES,
    OTHER_FIELDS,
    /// From here on it is `OBJECTS`. The first two are `LAYOUT`.
    SHAPE,
    STORAGE,
    PUBLIC_LENGTH,
    VECTOR_LENGTH,
    VALUE_ELEMENTS,
    DOUBLE_ELEMENTS,
}

const ROOM: u32 = 1 << 24;
const NAMED_START: u32 = ROOM;
const SCOPE_SLOTS_START: u32 = 2 * ROOM;
const GLOBAL_BINDINGS_START: u32 = 3 * ROOM;

fn range(start: u32, end: u32) -> HeapRange {
    HeapRange { start, end }
}

pub(crate) fn add_fixed(function: &mut Function) {
    let groups = [
        range(Position::STRING_FIBER as u32, u32::MAX),
        range(Position::SHAPE as u32, SCOPE_SLOTS_START),
        range(Position::PUBLIC_LENGTH as u32, Position::END as u32),
        range(NAMED_START, SCOPE_SLOTS_START),
        range(SCOPE_SLOTS_START, GLOBAL_BINDINGS_START),
        range(GLOBAL_BINDINGS_START, GLOBAL_BINDINGS_START + ROOM),
        range(Position::SHAPE as u32, Position::PUBLIC_LENGTH as u32),
    ];
    let leaves = (0..Position::END as u32).map(|position| range(position, position + 1));
    for range in groups.into_iter().chain(leaves) {
        function.add_heap(range);
    }
    debug_assert!(
        function.heap_count()
            == SHAPE.index() + (Position::END as usize - Position::SHAPE as usize)
    );
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Family {
    /// By the number of the name.
    Named,
    /// By the index of the slot.
    ScopeSlot,
    /// By the number of the binding.
    GlobalBinding,
}

pub(crate) fn add_member(function: &mut Function, family: Family, number: u32) -> Heap {
    let (start, whole) = match family {
        Family::Named => (NAMED_START, NAMED),
        Family::ScopeSlot => (SCOPE_SLOTS_START, SCOPE_SLOTS),
        Family::GlobalBinding => (GLOBAL_BINDINGS_START, GLOBAL_BINDINGS),
    };
    if number < ROOM {
        function.add_heap(range(start + number, start + number + 1))
    } else {
        whole
    }
}
