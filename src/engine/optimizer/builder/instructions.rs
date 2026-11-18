//! What each instruction becomes.

use bun_engine_asm::Cond;
use bun_engine_bytecode::{Instruction as I, IntCases, MetadataKind, Register};
use bun_engine_codegen::ir::{Binary, Type, Value as Ssa};
use bun_engine_vm::code::metadata::Flags;
use bun_engine_vm::{CallFrame, OFFSET_OF_INTERNAL_FIELDS, SpeculatedType as T};
use smallvec::SmallVec;

use super::calls::CallSite;
use super::frame::ArgumentCount;
use super::{Builder, Built, Refusal};
use crate::graph::{CallKind, ExitKind, Kind, Relation, heaps};

impl Builder<'_> {
    fn or_interpret(&mut self, is_done: bool) {
        if !is_done {
            self.interpret();
        }
    }

    fn set_boolean(&mut self, dst: Register, boolean: Option<Ssa>, is_negated: bool) {
        let Some(boolean) = boolean else {
            return self.interpret();
        };
        let boolean = if is_negated {
            self.not(boolean)
        } else {
            boolean
        };
        let value = self.box_boolean(boolean);
        self.set(dst, value);
    }

    fn jump_if(
        &mut self,
        condition: Option<Ssa>,
        expected: bool,
        target: bun_engine_bytecode::Jump,
        next: u32,
    ) {
        let Some(condition) = condition else {
            return self.interpret();
        };
        self.branch(condition, expected, target.from(self.frame().offset), next);
    }

    /// Without `this`, as an I32.
    fn argument_count(&mut self) -> Ssa {
        match self.frame().argument_count {
            ArgumentCount::Known(count) => self.int32(count as i32 - 1),
            ArgumentCount::InFrame => {
                let frame_pointer = self.frame_pointer();
                let count = self.load_immutable(
                    Type::I32,
                    frame_pointer,
                    CallFrame::OFFSET_OF_ARGUMENT_COUNT,
                );
                self.ir().sub(count, 1)
            }
        }
    }

    fn require(&mut self, value: Ssa, ty: T) {
        if !self.knowledge.type_of(&self.graph, value).intersects(ty) {
            self.exit_to_throw();
        } else if self.has_exited(ExitKind::BadType) && !self.is(value, ty) {
            self.interpret();
        } else {
            self.check_type(value, ty);
        }
    }

    pub(super) fn translate(&mut self, instruction: I, next: u32) -> Built {
        macro_rules! compare {
            ($op:expr, $relation:ident) => {{
                let result = self.compare(Relation::$relation, $op.lhs, $op.rhs);
                self.set_boolean($op.dst, result, false)
            }};
        }
        macro_rules! compare_and_jump {
            ($op:expr, $relation:ident, $expected:literal) => {{
                let result = self.compare(Relation::$relation, $op.lhs, $op.rhs);
                self.jump_if(result, $expected, $op.target, next)
            }};
        }
        macro_rules! set {
            ($op:expr, $boolean:expr, $is_negated:literal) => {{
                let boolean = $boolean;
                self.set_boolean($op.dst, boolean.into(), $is_negated)
            }};
        }
        macro_rules! jump {
            ($op:expr, $condition:expr, $expected:literal) => {{
                let condition = $condition;
                self.jump_if(condition.into(), $expected, $op.target, next)
            }};
        }
        macro_rules! attempt {
            ($done:expr) => {{
                let is_done = $done;
                self.or_interpret(is_done)
            }};
        }
        macro_rules! call {
            ($op:expr, $kind:ident, $metadata_kind:ident, $arguments:expr) => {{
                let (callee, arguments) = (self.get($op.callee), $arguments);
                return self.call(CallSite {
                    kind: CallKind::$kind,
                    dst: $op.dst,
                    callee,
                    arguments: &arguments,
                    metadata_kind: MetadataKind::$metadata_kind,
                    metadata: $op.metadata,
                });
            }};
        }
        macro_rules! call_with_frame {
            ($op:expr, $kind:ident, $metadata_kind:ident, $this:ident) => {
                call!($op, $kind, $metadata_kind, {
                    let this = self.get($op.$this);
                    self.outgoing_arguments(this, $op.frame, $op.count)
                })
            };
        }
        let code_block = self.frame().code_block;
        match instruction {
            I::Enter(op) => self.enter_function(op),
            I::Move(op) => {
                let value = self.get(op.src);
                self.set(op.dst, value);
            }
            I::LoopHint(_) => {
                self.node(Kind::PollTraps, &[], 0);
                if !self.is_full() {
                    self.node(Kind::TierUpInLoop, &[], 0);
                }
            }
            I::Return(op) => {
                let value = self.get(op.value);
                self.leave_with(value);
            }
            I::ToThis(op) => self.to_this(op.this),
            I::CreateThis(op) => self.create_this(op),
            I::CheckInitialized(op) => self.require(self.frame().get(op.value), T::HEAP_TOP),
            I::CheckThis(op) => self.require(self.frame().get(op.this), T::HEAP_TOP),
            I::BindThis(op) => {
                self.require(self.frame().get(op.this), T::EMPTY);
                if !self.block.is_none() {
                    self.set(op.this, self.frame().get(op.value));
                }
            }
            I::ArgumentCount(op) => {
                let count = self.argument_count();
                let count = self.box_int32(count);
                self.set(op.dst, count);
            }
            I::GetArgument(_)
            | I::CreateRest(_)
            | I::CreateDirectArguments(_)
            | I::CreateScopedArguments(_)
            | I::CreateUnmappedArguments(_)
            | I::EnsureArguments(_)
            | I::GetArgumentsLength(_)
            | I::GetArgumentByVal(_) => self.interpret_with_arguments(),

            I::CreateScope(op) => self.create_scope(op),
            I::GetScoped(op) => {
                self.get_scoped(op.dst, op.scope, 0, op.slot, Some(op.metadata));
            }
            I::GetOuter(op) => {
                self.get_scoped(op.dst, op.scope, op.depth, op.slot, Some(op.metadata));
            }
            I::GetScopedChecked(op) => {
                attempt!(self.get_scoped_checked(op.dst, op.scope, 0, op.slot, op.metadata))
            }
            I::GetOuterChecked(op) => {
                attempt!(self.get_scoped_checked(op.dst, op.scope, op.depth, op.slot, op.metadata))
            }
            I::GetInternal(op) => {
                self.get_scoped(op.dst, op.scope, op.depth, op.slot, None);
            }
            I::PutScoped(op) => self.put_scoped(op.scope, 0, op.slot, op.value),
            I::PutOuter(op) => self.put_scoped(op.scope, op.depth, op.slot, op.value),
            I::GetImport(op) => self.get_import(op),
            I::GetGlobal(op) => self.get_global(op),
            I::PutGlobal(op) => self.put_global(op.value, op.metadata),

            I::GetById(op) => self.get_cached(
                op.dst,
                op.base,
                code_block.identifier(op.name),
                MetadataKind::GetById,
                op.metadata,
            ),
            I::GetLength(op) => self.get_length(op),
            I::PutById(op) => self.put_by_id(op),
            I::GetByVal(op) => self.get_by_val(op),
            I::PutByVal(op) => self.put_by_val(op.base, op.key, op.value, op.metadata),
            I::PutByValDirect(op) => self.put_by_val(op.base, op.key, op.value, op.metadata),
            I::GetInternalField(op) => {
                if let Some(prediction) = self.prediction_or_exit(op.metadata) {
                    let base = self.get(op.base);
                    let (base, offset) = (
                        self.alias(base),
                        (OFFSET_OF_INTERNAL_FIELDS + op.index as usize * 8) as i32,
                    );
                    let value = self.load_value(base, offset, heaps::INTERNAL_FIELDS, prediction);
                    self.set(op.dst, value);
                }
            }
            I::PutInternalField(op) => {
                let (base, value) = (self.get(op.base), self.get(op.value));
                let (base, offset) = (
                    self.alias(base),
                    (OFFSET_OF_INTERNAL_FIELDS + op.index as usize * 8) as i32,
                );
                self.store_value(base, base, offset, heaps::INTERNAL_FIELDS, value);
            }

            I::NewObject(op) => self.new_object(op),
            I::NewObjectLiteral(op) => self.new_object_literal(op),
            I::NewFunction(op) => self.new_function(op),

            I::Call(op) => call_with_frame!(op, Call, Call, this),
            I::Call1(op) => call!(op, Call, Call, [self.get(op.this), self.get(op.first)]),
            I::Call2(op) => call!(
                op,
                Call,
                Call,
                [self.get(op.this), self.get(op.first), self.get(op.second)]
            ),
            I::TailCall(op) if bun_engine_vm::options().use_tail_calls => {
                call_with_frame!(op, TailCall, Call, this)
            }
            I::TailCall(op) => call_with_frame!(op, Call, Call, this),
            I::Construct(op) => call_with_frame!(op, Construct, Call, new_target),
            I::SuperConstruct(op) => call_with_frame!(op, Construct, SuperConstruct, new_target),
            I::CallVarargs(_)
            | I::TailCallVarargs(_)
            | I::ConstructVarargs(_)
            | I::SuperConstructVarargs(_)
            | I::CallWithSpread(_)
            | I::ResumeGenerator(_) => {
                if self.has_exited(ExitKind::NeverExecuted) {
                    return Err(Refusal::Unsupported);
                }
                self.force_exit(ExitKind::NeverExecuted);
            }

            I::Add(op) => {
                attempt!(self.number_binary(Kind::NumberAdd, op.dst, op.lhs, op.rhs, op.metadata))
            }
            I::Subtract(op) => attempt!(self.number_binary(
                Kind::NumberSubtract,
                op.dst,
                op.lhs,
                op.rhs,
                op.metadata
            )),
            I::Multiply(op) => attempt!(self.number_binary(
                Kind::NumberMultiply,
                op.dst,
                op.lhs,
                op.rhs,
                op.metadata
            )),
            I::Divide(op) => attempt!(self.number_binary(
                Kind::NumberDivide,
                op.dst,
                op.lhs,
                op.rhs,
                op.metadata
            )),
            I::Remainder(op) => attempt!(self.number_binary(
                Kind::NumberRemainder,
                op.dst,
                op.lhs,
                op.rhs,
                op.metadata
            )),
            I::BitAnd(op) => {
                attempt!(self.bitwise(Binary::And, op.dst, op.lhs, op.rhs, Some(op.metadata)))
            }
            I::BitOr(op) => {
                attempt!(self.bitwise(Binary::Or, op.dst, op.lhs, op.rhs, Some(op.metadata)))
            }
            I::BitXor(op) => {
                attempt!(self.bitwise(Binary::Xor, op.dst, op.lhs, op.rhs, Some(op.metadata)))
            }
            I::ShiftLeft(op) => {
                attempt!(self.bitwise(Binary::Shl, op.dst, op.lhs, op.rhs, Some(op.metadata)))
            }
            I::ShiftRight(op) => {
                attempt!(self.bitwise(Binary::Sar, op.dst, op.lhs, op.rhs, Some(op.metadata)))
            }
            I::ShiftRightUnsigned(op) => {
                attempt!(self.bitwise(Binary::Shr, op.dst, op.lhs, op.rhs, Some(op.metadata)))
            }
            I::ShiftRightUnsignedBits(op) => {
                attempt!(self.bitwise(Binary::Shr, op.dst, op.lhs, op.rhs, None))
            }
            I::BitNot(op) => attempt!(self.bit_not(op.dst, op.value, op.metadata)),
            I::Negate(op) => {
                attempt!(self.number_unary(Kind::NumberNegate, op.dst, op.value, op.metadata))
            }
            I::ToNumber(op) => {
                attempt!(self.number_unary(Kind::NumberToNumber, op.dst, op.value, op.metadata))
            }
            I::ToNumeric(op) => {
                attempt!(self.number_unary(Kind::NumberToNumber, op.dst, op.value, op.metadata))
            }
            I::Increment(op) => {
                attempt!(self.step(Kind::NumberAdd, op.dst, op.value, None, op.metadata))
            }
            I::Decrement(op) => {
                attempt!(self.step(Kind::NumberSubtract, op.dst, op.value, None, op.metadata))
            }
            I::PostIncrement(op) => attempt!(self.step(
                Kind::NumberAdd,
                op.value,
                op.value,
                Some(op.old),
                op.metadata
            )),
            I::PostDecrement(op) => attempt!(self.step(
                Kind::NumberSubtract,
                op.value,
                op.value,
                Some(op.old),
                op.metadata
            )),
            I::Not(op) => set!(
                op,
                {
                    let value = self.get(op.value);
                    self.to_boolean(value)
                },
                true
            ),

            I::Equal(op) => set!(op, self.loose_equal(op.lhs, op.rhs), false),
            I::NotEqual(op) => set!(op, self.loose_equal(op.lhs, op.rhs), true),
            I::StrictEqual(op) => set!(op, self.strict_equal(op.lhs, op.rhs), false),
            I::StrictNotEqual(op) => set!(op, self.strict_equal(op.lhs, op.rhs), true),
            I::Less(op) => compare!(op, Less),
            I::LessEqual(op) => compare!(op, LessEqual),
            I::Greater(op) => compare!(op, Greater),
            I::GreaterEqual(op) => compare!(op, GreaterEqual),
            I::Below(op) => set!(
                op,
                self.unsigned_compare(Cond::Below, op.lhs, op.rhs),
                false
            ),
            I::BelowEqual(op) => set!(
                op,
                self.unsigned_compare(Cond::BelowEq, op.lhs, op.rhs),
                false
            ),
            I::Identical(op) => set!(op, self.identical(op.lhs, op.rhs), false),
            I::NotIdentical(op) => set!(op, self.identical(op.lhs, op.rhs), true),
            I::EqualNull(op) => set!(op, self.test(Kind::EqualNull, op.value, 0), false),
            I::NotEqualNull(op) => set!(op, self.test(Kind::EqualNull, op.value, 0), true),
            I::IsNullish(op) => set!(op, self.test_type(op.value, T::NULLISH), false),
            I::IsBoolean(op) => set!(op, self.test_type(op.value, T::BOOLEAN), false),
            I::IsNumber(op) => set!(op, self.test_type(op.value, T::BOXED_NUMBER), false),
            I::IsObject(op) => set!(op, self.test_type(op.value, T::OBJECT), false),
            I::IsCellOfType(op) => set!(
                op,
                self.test(Kind::IsCellOfType, op.value, op.cell_type.0.into()),
                false
            ),

            I::Goto(op) => self.goto(self.target_at(op.target.from(self.frame().offset))),
            I::JumpIfTrue(op) => jump!(
                op,
                {
                    let value = self.get(op.value);
                    self.to_boolean(value)
                },
                true
            ),
            I::JumpIfFalse(op) => jump!(
                op,
                {
                    let value = self.get(op.value);
                    self.to_boolean(value)
                },
                false
            ),
            I::JumpIfEqual(op) => jump!(op, self.loose_equal(op.lhs, op.rhs), true),
            I::JumpIfNotEqual(op) => jump!(op, self.loose_equal(op.lhs, op.rhs), false),
            I::JumpIfStrictEqual(op) => jump!(op, self.strict_equal(op.lhs, op.rhs), true),
            I::JumpIfStrictNotEqual(op) => jump!(op, self.strict_equal(op.lhs, op.rhs), false),
            I::JumpIfLess(op) => compare_and_jump!(op, Less, true),
            I::JumpIfLessEqual(op) => compare_and_jump!(op, LessEqual, true),
            I::JumpIfGreater(op) => compare_and_jump!(op, Greater, true),
            I::JumpIfGreaterEqual(op) => compare_and_jump!(op, GreaterEqual, true),
            I::JumpIfNotLess(op) => compare_and_jump!(op, Less, false),
            I::JumpIfNotLessEqual(op) => compare_and_jump!(op, LessEqual, false),
            I::JumpIfNotGreater(op) => compare_and_jump!(op, Greater, false),
            I::JumpIfNotGreaterEqual(op) => compare_and_jump!(op, GreaterEqual, false),
            I::JumpIfBelow(op) => {
                jump!(op, self.unsigned_compare(Cond::Below, op.lhs, op.rhs), true)
            }
            I::JumpIfBelowEqual(op) => jump!(
                op,
                self.unsigned_compare(Cond::BelowEq, op.lhs, op.rhs),
                true
            ),
            I::JumpIfIdentical(op) => jump!(op, self.identical(op.lhs, op.rhs), true),
            I::JumpIfNotIdentical(op) => jump!(op, self.identical(op.lhs, op.rhs), false),
            I::JumpIfEqualNull(op) => jump!(op, self.test(Kind::EqualNull, op.value, 0), true),
            I::JumpIfNotEqualNull(op) => jump!(op, self.test(Kind::EqualNull, op.value, 0), false),
            I::JumpIfNullish(op) => jump!(op, self.test_type(op.value, T::NULLISH), true),
            I::JumpIfNotNullish(op) => jump!(op, self.test_type(op.value, T::NULLISH), false),
            I::JumpIfNumber(op) => jump!(op, self.test_type(op.value, T::BOXED_NUMBER), true),
            I::JumpIfNotNumber(op) => jump!(op, self.test_type(op.value, T::BOXED_NUMBER), false),
            I::JumpIfObject(op) => jump!(op, self.test_type(op.value, T::OBJECT), true),
            I::JumpIfNotObject(op) => jump!(op, self.test_type(op.value, T::OBJECT), false),
            I::JumpIfCellOfType(op) => jump!(
                op,
                self.test(Kind::IsCellOfType, op.value, op.cell_type.0.into()),
                true
            ),
            I::JumpIfNotCellOfType(op) => jump!(
                op,
                self.test(Kind::IsCellOfType, op.value, op.cell_type.0.into()),
                false
            ),
            I::JumpIfBuiltin(op) => {
                let has_differed = code_block
                    .checked_metadata::<Flags>(MetadataKind::Branch, op.metadata)
                    .0
                    .get()
                    != 0;
                if has_differed || self.has_exited(ExitKind::BadIdentity) {
                    jump!(op, self.identical(op.value.into(), op.builtin), true);
                } else {
                    let (value, builtin) = (self.get(op.value), self.get(op.builtin));
                    self.check_identical(value, builtin);
                    self.goto(self.target_at(op.target.from(self.frame().offset)));
                }
            }
            I::SwitchInt(op) => {
                let value = self.get(op.value);
                if !T::INT32.contains(self.expected(value)) || self.has_exited(ExitKind::BadType) {
                    self.interpret();
                } else {
                    let table = &code_block.body().int_switches()[op.table.index()];
                    let cases: SmallVec<[(i64, u32); 16]> = match &table.cases {
                        IntCases::Dense { first, targets } => {
                            (i64::from(*first)..).zip(targets.iter().copied()).collect()
                        }
                        IntCases::Sparse(cases) => cases
                            .iter()
                            .map(|&(key, target)| (key.into(), target))
                            .collect(),
                    };
                    let cases: SmallVec<[(i64, u32); 16]> = cases
                        .into_iter()
                        .filter(|&(_, target)| target != table.default)
                        .collect();
                    let value = self.as_int32(value);
                    self.switch(value, &cases, table.default);
                }
            }

            I::ThrowError(_) | I::ThrowStatic(_) => self.exit_to_throw(),
            I::RethrowIfAny(op) => self.require(self.frame().get(op.exception), T::EMPTY),
            I::ThrowIfNotObject(op) => {
                let value = self.get(op.value);
                self.require(value, T::OBJECT);
            }
            I::ThrowIfNullish(op) => {
                let value = self.get(op.value);
                self.require(value, !T::NULLISH);
            }
            I::ResumeDispatch(_)
            | I::Resume(_)
            | I::ResumeAfterYield(_)
            | I::ResumeAfterAwait(_)
            | I::ResumeAfterDelegate(_) => self.interpret_resumption(),
            _ => self.interpret(),
        }
        Ok(())
    }
}
