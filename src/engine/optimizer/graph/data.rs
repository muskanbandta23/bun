//! What the data of nodes means.

use bun_engine_bytecode::Register;
use bun_engine_vm::{
    Arithmetic, CellType, ElementKind, ExitKind, ExitKinds, IndexingMode, SpeculatedType,
};

use super::facts::ShapeIndex;

macro_rules! data_enums {
    ($($(#[$meta:meta])* $name:ident { $($variant:ident),* $(,)? })*) => {
        $($(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub(crate) enum $name {
            $($variant),*
        }

        impl $name {
            pub(crate) fn from_data(data: u32) -> Self {
                [$(Self::$variant),*][data as usize]
            }
        })*
    };
}

data_enums! {
    Relation { Less, LessEqual, Greater, GreaterEqual }
}

bitflags::bitflags! {
    /// What an operator has been seen to give.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(crate) struct ArithmeticData: u32 {
        const MAY_OVERFLOW_INT32 = 1 << 0;
        const MAY_OVERFLOW_INT52 = 1 << 1;
        const MAY_BE_DOUBLE = 1 << 2;
        const MAY_BE_NEGATIVE_ZERO = 1 << 3;
        /// An operand has been a double.
        const SAW_DOUBLE = 1 << 4;
    }
}

impl ArithmeticData {
    pub(crate) fn new(seen: Arithmetic, exits: ExitKinds) -> Self {
        let mut data = Self::empty();
        let overflowed = seen.contains(Arithmetic::RESULT_OVERFLOWED_INT32)
            || exits.contains(ExitKind::Overflow);
        data.set(Self::MAY_OVERFLOW_INT32, overflowed);
        data.set(
            Self::MAY_OVERFLOW_INT52,
            seen.contains(Arithmetic::RESULT_OVERFLOWED_INT52)
                || exits.contains(ExitKind::Int52Overflow),
        );
        data.set(
            Self::MAY_BE_DOUBLE,
            seen.contains(Arithmetic::RESULT_DOUBLE) || exits.contains(ExitKind::NotAnInteger),
        );
        data.set(
            Self::MAY_BE_NEGATIVE_ZERO,
            seen.contains(Arithmetic::RESULT_NEGATIVE_ZERO)
                || exits.contains(ExitKind::NegativeZero),
        );
        data.set(
            Self::SAW_DOUBLE,
            seen.intersects(Arithmetic::LHS_DOUBLE | Arithmetic::RHS_DOUBLE)
                || exits.contains(ExitKind::BadType),
        );
        data
    }
}

/// The bits of the indexing mode under a mask are as expected, or the cell is of a type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ElementCheck {
    Mode { mask: u8, expected: u8 },
    Type(CellType),
}

impl ElementCheck {
    pub(crate) fn dense(kind: ElementKind, is_array: Option<bool>, writes: bool) -> Self {
        let mut mask = if writes {
            IndexingMode::KIND_AND_WRITABILITY_MASK
        } else {
            IndexingMode::KIND_MASK
        };
        let mut expected = kind as u8;
        if let Some(is_array) = is_array {
            mask |= IndexingMode::IS_ARRAY;
            expected |= if is_array { IndexingMode::IS_ARRAY } else { 0 };
        }
        Self::Mode { mask, expected }
    }

    /// Never zero.
    pub(crate) fn to_data(self) -> u32 {
        match self {
            Self::Mode { mask, expected } => u32::from(mask) << 8 | u32::from(expected),
            Self::Type(cell_type) => 1 << 16 | u32::from(cell_type.0),
        }
    }

    pub(crate) fn from_data(data: u32) -> Self {
        if data >> 16 == 0 {
            Self::Mode {
                mask: (data >> 8) as u8,
                expected: data as u8,
            }
        } else {
            Self::Type(CellType(data as u8))
        }
    }

    pub(crate) fn implies(self, other: Self) -> bool {
        match (self, other) {
            (
                Self::Mode { mask, expected },
                Self::Mode {
                    mask: wanted_mask,
                    expected: wanted,
                },
            ) => wanted_mask & !mask == 0 && expected & wanted_mask == wanted,
            _ => self == other,
        }
    }

    pub(crate) fn holds_for(self, cell_type: CellType, mode: IndexingMode) -> bool {
        match self {
            Self::Mode { mask, expected } => mode.bits() & mask == expected,
            Self::Type(wanted) => cell_type == wanted,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct TypeCheck {
    pub(crate) wanted: SpeculatedType,
    /// What need not be looked for is not in here.
    pub(crate) known: SpeculatedType,
    pub(crate) failure: ExitKind,
}

#[derive(Default)]
pub(crate) struct TypeChecks(Vec<TypeCheck>);

impl TypeChecks {
    pub(crate) fn check(&mut self, check: TypeCheck) -> u32 {
        self.0
            .iter()
            .position(|&known| known == check)
            .unwrap_or_else(|| {
                self.0.push(check);
                self.0.len() - 1
            }) as u32
    }

    pub(crate) fn at(&self, data: u32) -> TypeCheck {
        self.0[data as usize]
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Transition {
    pub(crate) from: ShapeIndex,
    pub(crate) to: ShapeIndex,
}

#[derive(Clone, Copy)]
pub(crate) struct Allocation {
    pub(crate) shape: ShapeIndex,
    pub(crate) size: u32,
    /// The address of the `Allocator`, or zero if the cell is too large for one.
    pub(crate) allocator: usize,
}

/// The slots of the area that stands in for a frame when the interpreter executes an instruction.
#[derive(Clone, Copy, Default)]
pub(crate) struct ShadowFrame {
    pub(crate) lowest: i32,
    pub(crate) highest: i32,
}

impl ShadowFrame {
    pub(crate) fn include(&mut self, register: Register) {
        (self.lowest, self.highest) = (
            self.lowest.min(register.offset()),
            self.highest.max(register.offset()),
        );
    }
}
