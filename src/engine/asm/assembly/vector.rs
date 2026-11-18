use super::{FloatAssembly, ScratchNeeds};
use crate::label::Label;
use crate::operand::{Address, Amount, FloatWidth, Sign, Size, TestCond};
use crate::register::{Fpr, Gpr};
use crate::vector::{VectorBinary, VectorReduce, VectorShift, VectorTernary, VectorUnary};

/// A `Size` is that of an integer lane, a `FloatWidth` that of a float lane.
pub trait VectorAssembly: FloatAssembly {
    fn move_vector_constant(&mut self, dst: Fpr, bits: u128);
    /// Reads 8 bytes and doubles the size of each lane.
    fn load_vector_extend(&mut self, from: Size, sign: Sign, dst: Fpr, src: Address);
    fn load_vector_splat(&mut self, lanes: Size, dst: Fpr, src: Address);
    fn load_vector_lane(&mut self, lanes: Size, dst: Fpr, vector: Fpr, lane: u8, src: Address);
    fn store_vector_lane(&mut self, lanes: Size, dst: Address, vector: Fpr, lane: u8);
    fn splat_int(&mut self, lanes: Size, dst: Fpr, src: Gpr);
    fn splat_float(&mut self, lanes: FloatWidth, dst: Fpr, src: Fpr);
    /// Lanes of 8 and 16 bits are extended to 32.
    fn extract_int_lane(&mut self, lanes: Size, sign: Sign, dst: Gpr, vector: Fpr, lane: u8);
    fn extract_float_lane(&mut self, lanes: FloatWidth, dst: Fpr, vector: Fpr, lane: u8);
    fn replace_int_lane(&mut self, lanes: Size, dst: Fpr, vector: Fpr, lane: u8, value: Gpr);
    fn replace_float_lane(
        &mut self,
        lanes: FloatWidth,
        dst: Fpr,
        vector: Fpr,
        lane: u8,
        value: Fpr,
    );
    fn vector_unary(&mut self, op: VectorUnary, dst: Fpr, src: Fpr);
    fn vector_binary(&mut self, op: VectorBinary, dst: Fpr, lhs: Fpr, rhs: Fpr);
    fn vector_ternary(&mut self, op: VectorTernary, dst: Fpr, a: Fpr, b: Fpr, c: Fpr);
    fn vector_shift(&mut self, op: VectorShift, dst: Fpr, src: Fpr, amount: impl Into<Amount>);
    fn vector_reduce(&mut self, op: VectorReduce, dst: Gpr, src: Fpr);
    /// Byte `i` of `dst` is byte `lanes[i]` of the 32 bytes of `lhs` and `rhs`.
    fn vector_shuffle(&mut self, dst: Fpr, lhs: Fpr, rhs: Fpr, lanes: [u8; 16]);
    /// `cond` is `Zero` or `NonZero`.
    fn branch_test_vector(&mut self, cond: TestCond, vector: Fpr, target: Label);

    // What the operations above take from the scratch registers, however their operands coincide.

    fn scratch_for_unary(features: Self::Features, op: VectorUnary) -> ScratchNeeds;
    fn scratch_for_binary(features: Self::Features, op: VectorBinary) -> ScratchNeeds;
    fn scratch_for_ternary(features: Self::Features, op: VectorTernary) -> ScratchNeeds;
    fn scratch_for_shift(
        features: Self::Features,
        op: VectorShift,
        by_constant: bool,
    ) -> ScratchNeeds;
    fn scratch_for_reduce(features: Self::Features, op: VectorReduce) -> ScratchNeeds;
    fn scratch_for_shuffle(features: Self::Features, lanes: [u8; 16]) -> ScratchNeeds;
}
