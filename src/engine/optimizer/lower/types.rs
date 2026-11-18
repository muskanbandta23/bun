//! Telling what a tagged value is from its bits, and from the header of a cell.

use bun_engine_asm::{Cond, Sign, Size, abi};
use bun_engine_codegen::ir::{Access, AccessFlags, Value as Ssa};
use bun_engine_vm::{CellType, CellTypeRange, JsCell, SpeculatedType as T, Value};

use super::Lowering;
use crate::graph::heaps;

/// The parts that the bits of a tagged value fall into.
const IMMEDIATES: [(T, u64, u64); 4] = [
    (T::NULL, u64::MAX, Value::OTHER_TAG),
    (
        T::UNDEFINED,
        u64::MAX,
        Value::OTHER_TAG | Value::UNDEFINED_TAG,
    ),
    (T::BOOLEAN, !1, Value::OTHER_TAG | Value::BOOL_TAG),
    (T::NULLISH, !Value::UNDEFINED_TAG, Value::OTHER_TAG),
];

/// The cell types that a set of speculated types covers, where they make one range.
fn cell_range(wanted: T) -> Option<CellTypeRange> {
    let ranges = [
        (T::OBJECT, CellType::OBJECTS),
        (T::TYPED_ARRAY, CellType::TYPED_ARRAYS),
        (T::FUNCTION, CellType::FUNCTIONS),
        (
            T::DIRECT_ARGUMENTS | T::SCOPED_ARGUMENTS,
            CellType::ARGUMENTS,
        ),
    ];
    let single = |ty: T, cell_type: CellType| (ty, CellTypeRange::new(cell_type, cell_type));
    let singles = [
        single(T::STRING, CellType::String),
        single(T::SYMBOL, CellType::Symbol),
        single(T::BIGINT, CellType::BigInt),
        single(T::FINAL_OBJECT, CellType::FinalObject),
        single(T::ARRAY, CellType::Array),
        single(T::DERIVED_ARRAY, CellType::DerivedArray),
        single(T::REGEXP_OBJECT, CellType::RegExpObject),
        single(T::DATE, CellType::Date),
        single(T::PROMISE, CellType::Promise),
        single(T::MAP, CellType::Map),
        single(T::SET, CellType::Set),
        single(T::MAP_ITERATOR, CellType::MapIterator),
        single(T::SET_ITERATOR, CellType::SetIterator),
        single(T::WEAK_MAP, CellType::WeakMap),
        single(T::WEAK_SET, CellType::WeakSet),
        single(T::PROXY, CellType::Proxy),
        single(T::GLOBAL_PROXY, CellType::GlobalProxy),
        single(T::STRING_OBJECT, CellType::StringObject),
    ];
    ranges
        .into_iter()
        .chain(singles)
        .find(|&(ty, _)| ty == wanted)
        .map(|(_, range)| range)
}

impl Lowering<'_> {
    /// Whether `value` is of one of `wanted`, given that it is of one of `known`. `None` where the bits do not
    /// tell them apart.
    pub(super) fn test_type(&mut self, value: Ssa, wanted: T, known: T) -> Option<Ssa> {
        let wanted = wanted & known;
        if wanted == known {
            return Some(self.int32(1));
        }
        if wanted.is_empty() {
            return Some(self.int32(0));
        }
        if let Some(test) = self.test_whole_group(value, wanted, known) {
            return Some(test);
        }
        // What is left needs the header of a cell, so everything else has to be ruled out first.
        let cell_part = wanted & T::CELL;
        if wanted != cell_part || cell_range(cell_part).is_none() {
            return None;
        }
        let is_cell = self.test_whole_group(value, T::CELL, known)?;
        let of_type = self.test_cell_type(value, cell_part)?;
        Some(self.ir().and(is_cell, of_type))
    }

    /// A test that one compare makes, for the groups that the tag tells apart.
    fn test_whole_group(&mut self, value: Ssa, wanted: T, known: T) -> Option<Ssa> {
        let numbers = T::INT32 | T::FULL_DOUBLE;
        if wanted == T::INT32 & known {
            let tag = self.tag(abi::NUMBER_TAG);
            return Some(self.ir().compare(Cond::AboveEq, value, tag));
        }
        if wanted == numbers & known {
            let tag = self.tag(abi::NUMBER_TAG);
            let bits = self.ir().and(value, tag);
            return Some(self.ir().is_not_zero(bits));
        }
        if wanted == T::FULL_DOUBLE & known && !(known & T::INT32).is_empty() {
            let tag = self.tag(abi::NUMBER_TAG);
            let bits = self.ir().and(value, tag);
            let is_number = self.ir().is_not_zero(bits);
            let is_int32 = self.ir().compare(Cond::AboveEq, value, tag);
            let is_not_int32 = self.ir().xor(is_int32, 1);
            return Some(self.ir().and(is_number, is_not_int32));
        }
        if wanted == T::CELL & known {
            let mask = self.tag(abi::NOT_CELL_MASK);
            let bits = self.ir().and(value, mask);
            let is_pointer = self.ir().is_zero(bits);
            if known.contains(T::EMPTY) {
                let is_empty = self.ir().is_zero(value);
                let is_set = self.ir().xor(is_empty, 1);
                return Some(self.ir().and(is_pointer, is_set));
            }
            return Some(is_pointer);
        }
        if wanted == T::EMPTY & known {
            return Some(self.ir().is_zero(value));
        }
        for (ty, mask, expected) in IMMEDIATES {
            if wanted == ty & known {
                let bits = self.ir().and(value, mask as i64);
                return Some(self.ir().equal(bits, expected as i64));
            }
        }
        None
    }

    /// Whether the cell is of one of the types. The value has to be a cell already.
    fn test_cell_type(&mut self, cell: Ssa, wanted: T) -> Option<Ssa> {
        let range = cell_range(wanted)?;
        let cell_type = self.load_cell_type(cell);
        if range.first == range.last {
            return Some(self.ir().equal(cell_type, i32::from(range.first.0)));
        }
        let from_first = self.ir().sub(cell_type, i32::from(range.first.0));
        let span = i32::from(range.last.0) - i32::from(range.first.0);
        Some(self.ir().below_or_equal(from_first, span))
    }

    pub(super) fn load_cell_type(&mut self, cell: Ssa) -> Ssa {
        let access = Access::new(heaps::SHAPE, AccessFlags::ALWAYS_READABLE);
        self.ir().load_narrow(
            Size::S8,
            Sign::Unsigned,
            cell,
            JsCell::OFFSET_OF_TYPE as i32,
            access,
        )
    }
}
