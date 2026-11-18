//! Instructions without their registers. Comments list the operands. Those of an address come last.
//!
//! Moves leave the flags alone, so one instruction can set them for the next.

use bun_engine_asm::arm64::{
    AccessSize, AddSub, ArithmeticImm, AtomicUpdate, Barrier, Bitfield, BitmaskImm, Cc,
    ConditionalCompare, Extend, FP, FloatBinary, FloatFlags, FloatUnary, IndexExtend, Logical,
    MultiplyAdd, MultiplyAddLong, NeonBitwise, Nzcv, Select, ShiftOp, Unary,
};
use bun_engine_asm::{Arm64Features, AtomicOp, FloatWidth, Gpr, Sign, Size, Width};

use super::address::Neighbours;
use crate::ir::{Type, Value, Wide};
use crate::machine::{Instruction, Lowering, Shared, VectorInstruction};

/// Stands for the zero register among the operands.
pub(super) const ZERO: u8 = 64;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Amode {
    /// Base. The offset need not fit in the instruction.
    Offset(i32),
    /// From the frame pointer.
    Frame(i32),
    /// Base, index. The index may be multiplied by the size of the access.
    Indexed(IndexExtend, bool),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Mem {
    pub(crate) size: AccessSize,
    /// May be done by one instruction with its neighbour.
    pub(crate) may_pair: bool,
    pub(crate) address: Amode,
}

/// The right side of an addition, subtraction or comparison.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Addend {
    Shifted(ShiftOp, u8),
    Extended(Extend, u8),
    Imm(ArithmeticImm),
}

impl Addend {
    pub(super) const PLAIN: Self = Self::Shifted(ShiftOp::Lsl, 0);
}

/// The right side of a logical operation.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Mask {
    Shifted(Width, ShiftOp, u8),
    Imm(BitmaskImm),
}

/// What decides a jump.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Test {
    Flags(FloatFlags),
    /// One operand.
    Zero(Width),
    NotZero(Width),
    BitClear(u8),
    BitSet(u8),
    VectorZero,
    VectorNotZero,
    /// Value, number of the bit.
    NumberedBitClear(Width),
    NumberedBitSet(Width),
}

impl Test {
    pub(super) fn inverted(self) -> Self {
        match self {
            Self::Flags(flags) => Self::Flags(flags.inverted()),
            Self::Zero(width) => Self::NotZero(width),
            Self::NotZero(width) => Self::Zero(width),
            Self::BitClear(bit) => Self::BitSet(bit),
            Self::BitSet(bit) => Self::BitClear(bit),
            Self::VectorZero => Self::VectorNotZero,
            Self::VectorNotZero => Self::VectorZero,
            Self::NumberedBitClear(width) => Self::NumberedBitSet(width),
            Self::NumberedBitSet(width) => Self::NumberedBitClear(width),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Inst {
    Copy,
    ZeroExtend,
    Jump,
    Return,
    Unreachable,
    Patchpoint,
    StoreOutgoing(i32, Type),
    LoadOutgoing(i32, Type),
    StoreResult(i32, Type),
    Shared(Shared),
    Vector(VectorInstruction),

    /// Result, left, right.
    AddSub(AddSub, Width, Addend),
    Logical(Logical, Mask),
    /// By a register, or by its negative.
    Shift(ShiftOp, Width, bool),
    Extract(Width, u8),
    MultiplyHigh(Sign),
    /// Of 32 bits each, and shifted right.
    MultiplyLongAndShift(Sign, u8),
    Divide(Sign, Width),
    Remainder(Sign, Width),
    TotalRemainder(Width),
    CheckedAddSub(AddSub, Width, Addend),
    CheckedMul(Width),
    /// Result, left, right, addend.
    MultiplyAdd(MultiplyAdd, Width),
    MultiplyAddLong(MultiplyAddLong),
    /// Result, source.
    AddConstant(Width, i32),
    Bitfield(Bitfield, Width, u8, u8),
    Unary(Unary, Width),
    CountTrailingZeros(Width),
    CheckedNeg(Width),
    CheckedShift(Width, u8),
    /// Result, what is kept, what is inserted.
    InsertBits(Width, u8, u8),
    /// Result.
    FrameAddress(i32),
    /// Low and high result, left low and high, right low and high.
    Wide(Wide),
    /// Low and high result, left, right.
    MulWide(Sign),

    /// Result.
    Load(Mem),
    LoadSigned(Width, Mem),
    LoadFloat(Mem),
    LoadAndExtendLanes(Size, Sign, Mem),
    LoadSplat(Size, Amode),
    LoadAcquire(Size, Sign, Amode),
    /// Result, vector.
    LoadLane(Size, u8, Amode),
    /// Value.
    Store(Mem),
    StoreFloat(Mem),
    StoreLane(Size, u8, Amode),
    StoreRelease(Size, Amode),
    /// Result, operand, address. The operand is first subtracted from zero or inverted if the operation says so.
    AtomicUpdate(AtomicUpdate, Size, Option<AtomicOp>),
    AtomicLoop(AtomicOp, Size),
    /// Result, expected, replacement, address.
    AtomicCompareExchange(Size),
    Fence(Barrier),

    /// Result, left, right.
    FloatBinary(FloatBinary, FloatWidth),
    FloatBits(NeonBitwise),
    /// Result, source.
    FloatUnary(FloatUnary, FloatWidth),
    Bitcast(Type),
    IntToFloat(Sign, Width, FloatWidth),
    FloatToInt32Modular,

    // These set the flags.
    /// Left, right.
    Compare(AddSub, Width, Addend),
    TestBits(Logical, Mask),
    TestNumberedBit(Width),
    CompareFloats(FloatWidth),
    CompareIf(ConditionalCompare, Width, Option<u8>, Nzcv, Cc),
    CompareFloatsIf(FloatWidth, Nzcv, Cc),
    /// Left.
    CompareFloatWithZero(FloatWidth),

    Branch(Test),
    Check(Test),
    /// Result, then, else.
    Select(Select, Width, Cc),
    SelectOnEither(Width, Cc, Cc),
    SelectFloat(FloatWidth, FloatFlags),
    SelectVector(FloatFlags),
    /// Result.
    SetOnEither(Cc, Cc),
    /// Index.
    JumpTable,
}

impl Instruction for Inst {
    type Features = Arm64Features;
    type Selection = Neighbours;

    const COPY: Self = Self::Copy;
    const ZERO_EXTEND: Self = Self::ZeroExtend;
    const JUMP: Self = Self::Jump;
    const RETURN: Self = Self::Return;
    const UNREACHABLE: Self = Self::Unreachable;
    const PATCHPOINT: Self = Self::Patchpoint;
    const FRAME_POINTER: Gpr = FP;

    fn store_outgoing(offset: i32, ty: Type) -> Self {
        Self::StoreOutgoing(offset, ty)
    }

    fn load_outgoing(offset: i32, ty: Type) -> Self {
        Self::LoadOutgoing(offset, ty)
    }

    fn store_result(offset: i32, ty: Type) -> Self {
        Self::StoreResult(offset, ty)
    }

    fn lower(lowering: &mut Lowering<'_, Self>, value: Value) {
        lowering.select(value);
    }
}
