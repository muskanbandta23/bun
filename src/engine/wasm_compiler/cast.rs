//! What has to be checked to know that a reference has a type.

use bun_engine_wasm_format::{AbstractHeapType, RefType, Rtt, TypeIndex, TypeKind, Types};

use crate::abi::{INT32_TAG, NULL};

/// The bits of which a cell has none.
pub const NOT_CELL_MASK: u64 = INT32_TAG | NULL as u64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Null {
    Impossible,
    Passes,
    Fails,
}

/// What a reference that is not null can be, and so how to tell an `i31ref` and a cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// An `i31ref` is not below [`INT32_TAG`]. A cell has no bit of [`NOT_CELL_MASK`], and is of any kind.
    Any,
    /// An `i31ref` is negative. The rest are structs and arrays.
    Eq,
    /// An object with an `Rtt`.
    Object,
}

/// For a reference that is not null.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Check {
    Passes,
    Fails,
    IsI31(Source),
    /// From [`Source::Any`]: an `i31ref`, a struct or an array.
    IsEq,
    /// A struct or an array, as the kind of the cell says.
    IsKind(Source, TypeKind),
    /// From [`Source::Any`] the kind of the cell has to be checked before the `Rtt` is read.
    IsDefined(Source, TypeKind, RttCheck),
}

/// For the `Rtt` of an object. `expected` is an address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RttCheck {
    Is {
        expected: usize,
    },
    /// The word at this offset in it is.
    InlineEntryIs {
        offset: i32,
        expected: usize,
    },
    /// Its depth is not less, and the entry for that depth in the long display is.
    LongEntryIs {
        depth: u32,
        expected: usize,
    },
}

impl RttCheck {
    pub fn new(types: &Types, ty: TypeIndex) -> Self {
        let (expected, depth) = (types.rtt(ty).address(), types.depth(ty));
        if types.is_final(ty) {
            Self::Is { expected }
        } else if depth < Rtt::INLINE_DISPLAY_LENGTH {
            let offset = Rtt::DISPLAY_OFFSET + depth as i32 * 8;
            Self::InlineEntryIs { offset, expected }
        } else {
            Self::LongEntryIs { depth, expected }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cast {
    pub null: Null,
    pub check: Check,
}

impl Cast {
    /// Both types are of the same hierarchy.
    pub fn new(types: &Types, from: RefType, to: RefType) -> Self {
        let null = match (from.is_nullable(), to.is_nullable()) {
            (false, _) => Null::Impossible,
            (true, true) => Null::Passes,
            (true, false) => Null::Fails,
        };
        let (from, to) = (from.heap_type(), to.heap_type());
        let source = match from.abstract_type() {
            AbstractHeapType::Any => Source::Any,
            AbstractHeapType::Eq => Source::Eq,
            _ => Source::Object,
        };
        let check = if types.is_heap_subtype(from, to) {
            Check::Passes
        } else if !types.is_heap_subtype(to, from) {
            Check::Fails
        } else if let Some(index) = to.index() {
            Check::IsDefined(source, types.kind(index), RttCheck::new(types, index))
        } else {
            match to.abstract_type() {
                AbstractHeapType::Eq => Check::IsEq,
                AbstractHeapType::I31 => Check::IsI31(source),
                AbstractHeapType::Struct => Check::IsKind(source, TypeKind::Struct),
                AbstractHeapType::Array => Check::IsKind(source, TypeKind::Array),
                _ => Check::Fails,
            }
        };
        Self { null, check }
    }
}
