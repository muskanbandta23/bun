//! The instructions that know of JavaScript. They share a function with those of the back end.
//!
//! Operands and results are tagged values unless an entry says otherwise. A check gives its first operand back, and
//! what is valid only after the check uses that result. A node that may exit or throw has a state.

use bun_engine_codegen::ir::{EffectFlags, Effects, Heap, NodeInfo, NodeKind, Type};
use bun_engine_vm::SpeculatedType as T;

use super::heaps::*;

#[derive(Clone, Copy)]
struct Does {
    effects: Effects,
    collects: bool,
    throws: bool,
}

const NOTHING: Does = Does {
    effects: Effects::NONE,
    collects: false,
    throws: false,
};
const EXIT: Does = NOTHING.and(EffectFlags::EXITS);
/// Runs scripts.
const ANYTHING: Does = Does {
    effects: Effects {
        reads: WORLD,
        writes: WORLD,
        ..Effects::CALL
    },
    collects: true,
    throws: true,
};
/// Two allocations are not the same, and one that is not used is not needed.
const ALLOCATE: Does = Does {
    collects: true,
    ..NOTHING.and(EffectFlags::DISTINCT)
};

impl Does {
    const fn and(mut self, flags: EffectFlags) -> Self {
        self.effects.flags = self.effects.flags.union(flags);
        self
    }

    const fn reading(mut self, heap: Heap) -> Self {
        self.effects.reads = heap;
        self.and(EffectFlags::CONTROL_DEPENDENT)
    }

    const fn writing(mut self, heap: Heap) -> Self {
        self.effects.writes = heap;
        self.and(EffectFlags::CONTROL_DEPENDENT)
    }

    const fn collecting(self) -> Self {
        Self {
            collects: true,
            ..self
        }
    }

    const fn throwing(self) -> Self {
        Self {
            collects: true,
            throws: true,
            ..self.and(EffectFlags::EXITS)
        }
    }

    const fn on_frame(self) -> Self {
        self.reading(Heap::EVERYTHING).writing(Heap::EVERYTHING)
    }

    const fn ending(self) -> Self {
        self.and(EffectFlags::ENDS_BLOCK)
    }
}

/// What a node gives.
#[derive(Clone, Copy)]
pub(crate) enum Gives {
    Nothing,
    /// A tagged value of this type.
    Value(T),
    /// The first operand.
    Operand,
    Int32,
    Boolean,
    Int52,
    Double,
    /// An I64 that is no value of the language.
    Word,
    Tuple(&'static [Type]),
}

impl Gives {
    pub(crate) const fn machine_type(self) -> Type {
        match self {
            Self::Nothing => Type::Void,
            Self::Value(_) | Self::Operand | Self::Int52 | Self::Word => Type::I64,
            Self::Int32 | Self::Boolean => Type::I32,
            Self::Double => Type::F64,
            Self::Tuple(_) => Type::Tuple,
        }
    }
}

const TOP: Gives = Gives::Value(T::HEAP_TOP);
const NUMBER: Gives = Gives::Value(T::BOXED_NUMBER);
const STRING: Gives = Gives::Value(T::STRING);
const OBJECT: Gives = Gives::Value(T::OBJECT);

macro_rules! nodes {
    ($($(#[$meta:meta])* $name:ident: $gives:expr, $does:expr;)*) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        #[repr(u16)]
        pub(crate) enum Kind {
            $($(#[$meta])* $name,)*
        }

        const ALL: &[Kind] = &[$(Kind::$name,)*];
        const GIVES: &[Gives] = { use Gives::*; &[$($gives,)*] };
        const DOES: &[Does] = &[$($does,)*];
        pub(crate) static INFO: &[NodeInfo] = &[$(NodeInfo {
            name: stringify!($name),
            effects: DOES[Kind::$name as usize].effects,
            elements: if let Gives::Tuple(elements) = GIVES[Kind::$name as usize] { elements } else { &[] },
        },)*];
    };
}

impl Kind {
    #[inline]
    pub(crate) fn of(kind: NodeKind) -> Self {
        ALL[usize::from(kind.0)]
    }

    #[inline]
    pub(crate) fn id(self) -> NodeKind {
        NodeKind(self as u16)
    }

    #[inline]
    pub(crate) fn gives(self) -> Gives {
        GIVES[self as usize]
    }

    #[inline]
    pub(crate) fn name(self) -> &'static str {
        INFO[self as usize].name
    }

    pub(crate) fn all() -> &'static [Kind] {
        ALL
    }

    #[inline]
    pub(crate) fn effects(self) -> Effects {
        DOES[self as usize].effects
    }

    #[inline]
    pub(crate) fn can_collect(self) -> bool {
        DOES[self as usize].collects
    }

    #[inline]
    pub(crate) fn can_throw(self) -> bool {
        DOES[self as usize].throws
    }
}

nodes! {
    // Places

    /// The first instruction of a block with parameters. Its state is valid on the edges to the block, with their
    /// arguments for the parameters.
    Checkpoint: Nothing, NOTHING.and(EffectFlags::KEPT);
    /// Where the code is left once an assumption has failed.
    InvalidationPoint: Nothing, EXIT.writing(SIDE_STATE);
    /// The data is an `ExitKind`.
    ForceExit: Nothing, EXIT.ending();
    PollTraps: Nothing, EXIT.writing(SIDE_STATE).throwing();
    /// The data is the offset of the loop.
    TierUpInLoop: Nothing, EXIT.writing(SIDE_STATE).collecting();
    TierUpAtReturn: Nothing, NOTHING.writing(SIDE_STATE).collecting();
    /// The operand, below a branch that tells something about it. The data is a `TypeCheck`.
    Refine: Operand, NOTHING.and(EffectFlags::CONTROL_DEPENDENT);
    /// Follows the store of the second operand, if there is one, into the first.
    StoreBarrier: Nothing, NOTHING.writing(SIDE_STATE);
    /// Between making an object and storing a reference to it.
    PublishFence: Nothing, NOTHING.and(EffectFlags::FENCE);

    // Representations

    BoxInt32: Value(T::INT32), NOTHING;
    BoxInt52: NUMBER, NOTHING;
    BoxDouble: Value(T::BOXED_DOUBLE), NOTHING;
    BoxBoolean: Value(T::BOOLEAN), NOTHING;
    /// Of an I32 taken as unsigned. The data says whether it has been seen to need a double.
    UInt32ToNumber: NUMBER, EXIT;
    AsInt32: Int32, EXIT;
    AsInt52: Int52, EXIT;
    AsBoolean: Boolean, EXIT;
    /// Of a number.
    AsDouble: Double, EXIT;
    /// ToInt32 of a number, a boolean, undefined or null.
    Truncate: Int32, EXIT;
    /// Of an operand that is known to be a number. The data is whether it is expected to be an int32.
    NumberToDouble: Double, NOTHING;
    /// Of a number, a boolean, undefined or null.
    PrimitiveToDouble: Double, NOTHING;
    /// ToInt32 of an F64.
    TruncateDouble: Int32, NOTHING;
    /// Of an F64 that has to be an integer other than −0.
    DoubleAsInt32: Int32, EXIT;
    DoubleAsInt52: Int52, EXIT;

    // Checks

    /// The data is a `TypeCheck`.
    CheckType: Operand, EXIT;
    /// Of a cell. The data is a `ShapeSet`.
    CheckShape: Operand, EXIT.reading(SHAPE);
    /// Of a cell. The data is an `ElementCheck`.
    CheckElements: Operand, EXIT.reading(SHAPE);
    /// The second operand is a constant with the same bits.
    CheckIdentical: Operand, EXIT;
    /// Of a function. The second operand is the executable.
    CheckExecutable: Operand, EXIT.reading(FUNCTION_TARGET);

    // Operators on numbers. What is no number exits. The data is an `ArithmeticData`.

    NumberAdd: NUMBER, EXIT;
    NumberSubtract: NUMBER, EXIT;
    NumberMultiply: NUMBER, EXIT;
    NumberDivide: NUMBER, EXIT;
    NumberRemainder: NUMBER, EXIT;
    NumberNegate: NUMBER, EXIT;
    NumberToNumber: NUMBER, EXIT;
    /// Of two F64.
    DoubleRemainder: Double, NOTHING;

    // Operators on what converts without running scripts. Anything else exits.

    /// The data is a `Relation`.
    Compare: Boolean, EXIT.reading(STRING_FIBER).collecting();
    LooseEqual: Boolean, EXIT.reading(STRING_FIBER).collecting();
    StrictEqual: Boolean, EXIT.reading(STRING_FIBER).collecting();
    ToBoolean: Boolean, EXIT.reading(STRING_FIBER);
    EqualNull: Boolean, EXIT;
    /// The data is a `TypeCheck`.
    IsType: Boolean, NOTHING;
    IsCellOfType: Boolean, NOTHING;
    /// What specialization leaves of the comparisons where it takes more than an instruction.
    StringEqual: Boolean, NOTHING.reading(STRING_FIBER).throwing();
    AtomEqual: Boolean, NOTHING;
    PrimitiveCompare: Boolean, NOTHING.reading(STRING_FIBER).throwing();
    QuietLooseEqual: Boolean, NOTHING.reading(STRING_FIBER).throwing();
    GenericEqualNull: Boolean, NOTHING;
    GenericStrictEqual: Boolean, NOTHING.reading(STRING_FIBER).throwing();
    GenericToBoolean: Boolean, NOTHING.reading(STRING_FIBER);

    // Objects

    /// Of a cell.
    LoadShapeId: Int32, NOTHING.reading(SHAPE);
    /// The data is a position in `Graph::transitions`.
    SetShape: Nothing, NOTHING.writing(SHAPE);
    /// Moves the slots to a storage with room for those of the new shape, and marks the old shape as no longer
    /// describing the object. `SetShape` follows. Gives the storage.
    GrowSlots: Word, NOTHING.writing(LAYOUT).collecting();
    /// The data of these is a position in `Graph::allocations`. The slots are empty.
    NewObject: Value(T::FINAL_OBJECT), ALLOCATE;
    /// Parent and template. Stores that follow initialize the slots.
    NewScope: Value(T::OTHER_CELL), ALLOCATE;
    /// Scope and executable.
    NewFunction: Value(T::FUNCTION), ALLOCATE;

    // Elements

    /// Array, storage, I32 index, and the value in the form of the elements. The index may be past the length, and the
    /// data is the `ElementKind`.
    StoreElementGrowing: Nothing, NOTHING.writing(OBJECTS).throwing();
    /// Of a string.
    StringLength: Int32, NOTHING.and(EffectFlags::CONTROL_DEPENDENT);
    ResolveRope: Operand, NOTHING.writing(STRING_FIBER).throwing();
    /// Of a string that is no rope, at an I32 index that is in bounds.
    StringUnitAt: Int32, NOTHING.reading(STRING_FIBER);
    /// From an I32 code unit.
    StringFromUnit: STRING, NOTHING.collecting();
    /// Of two or three strings.
    Concatenate: STRING, ALLOCATE.throwing();

    // Calls. The operands are the callee, `this` or `new.target`, and the arguments.

    Call: TOP, ANYTHING;
    Construct: OBJECT, ANYTHING;
    TailCall: Nothing, ANYTHING.ending();
    /// The executable comes first, and there are no fewer arguments than parameters. The data is their real number.
    DirectCall: TOP, ANYTHING;
    DirectConstruct: OBJECT, ANYTHING;
    DirectTailCall: Nothing, ANYTHING.ending();

    // Instructions that the interpreter executes

    /// What stands in for the frame pointer.
    ShadowFrame: Word, NOTHING;
    /// The shadow frame and the address of the instruction. The data is the opcode. Gives an `Outcome`.
    Interpret: Tuple(&[Type::I64, Type::I64]), ANYTHING.on_frame();
}
