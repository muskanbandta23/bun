//! Operators.

use bun_engine_bytecode::{MetadataKind, Operand, Register};
use bun_engine_codegen::ir::{Binary, Value as Ssa};
use bun_engine_vm::{Arithmetic, ArithmeticProfile, SpeculatedType as T, Value};

use super::Builder;
use crate::graph::{ArithmeticData, ExitKind, Kind, Relation};

const CONVERTS_QUIETLY: T = T::BOXED_NUMBER.union(T::BOOLEAN).union(T::NULLISH);

impl Builder<'_> {
    fn seen(&self, kind: MetadataKind, metadata: u32) -> Arithmetic {
        self.frame()
            .code_block
            .checked_metadata::<ArithmeticProfile>(kind, metadata)
            .get()
    }

    fn data_for(&self, seen: Arithmetic) -> u32 {
        ArithmeticData::new(seen, self.graph.site(self.origin).exits).bits()
    }

    fn may_be_numbers(&self, seen: Arithmetic, operands: &[Ssa]) -> bool {
        let is_number = |&operand: &Ssa| T::BOXED_NUMBER.contains(self.expected(operand));
        !seen.intersects(Arithmetic::LHS_OTHER | Arithmetic::RHS_OTHER)
            && !self.has_exited(ExitKind::ExoticObject)
            && operands.iter().all(is_number)
    }

    /// Returns false if the interpreter has to do it.
    pub(super) fn number_binary(
        &mut self,
        kind: Kind,
        dst: Register,
        lhs: Operand,
        rhs: Operand,
        metadata: u32,
    ) -> bool {
        let (seen, operands) = (
            self.seen(MetadataKind::BinaryArithmetic, metadata),
            [self.get(lhs), self.get(rhs)],
        );
        if kind == Kind::NumberAdd && self.concatenates(dst, operands) {
            return true;
        }
        let is_possible = self.may_be_numbers(seen, &operands);
        if is_possible {
            let result = self.number_node(kind, &operands, seen);
            self.set(dst, result);
        }
        is_possible
    }

    fn number_node(&mut self, kind: Kind, operands: &[Ssa], seen: Arithmetic) -> Ssa {
        let roots: smallvec::SmallVec<[Ssa; 2]> = operands
            .iter()
            .map(|&operand| self.graph.root(operand))
            .collect();
        let result = self.node(kind, &roots, self.data_for(seen));
        self.graph.predict(result, T::empty());
        result
    }

    fn concatenates(&mut self, dst: Register, [lhs, rhs]: [Ssa; 2]) -> bool {
        let is_string = |operand| {
            !self.expected(operand).is_empty() && T::STRING.contains(self.expected(operand))
        };
        if !is_string(lhs) || !is_string(rhs) || self.has_exited(ExitKind::BadType) {
            return false;
        }
        let operands = [
            self.check_type(lhs, T::STRING),
            self.check_type(rhs, T::STRING),
        ];
        let result = self.node(Kind::Concatenate, &operands, 0);
        self.set(dst, result);
        true
    }

    fn truncates_quietly(&self, seen: Arithmetic, operands: &[Ssa]) -> bool {
        !seen.intersects(Arithmetic::RESULT_BIGINT)
            && !self.has_exited(ExitKind::ExoticObject)
            && operands
                .iter()
                .all(|&operand| CONVERTS_QUIETLY.contains(self.expected(operand)))
    }

    pub(super) fn bitwise(
        &mut self,
        op: Binary,
        dst: Register,
        lhs: Operand,
        rhs: Operand,
        metadata: Option<u32>,
    ) -> bool {
        let seen = metadata.map_or(Arithmetic::empty(), |metadata| {
            self.seen(MetadataKind::BinaryArithmetic, metadata)
        });
        let operands = [self.get(lhs), self.get(rhs)];
        if !self.truncates_quietly(seen, &operands) {
            return false;
        }
        let [lhs, rhs] = operands.map(|operand| self.truncate(operand));
        let bits = self.ir().binary(op, lhs, rhs);
        let result = if op == Binary::Shr && metadata.is_some() {
            self.number_node(Kind::UInt32ToNumber, &[bits], seen)
        } else {
            self.box_int32(bits)
        };
        self.set(dst, result);
        true
    }

    pub(super) fn bit_not(&mut self, dst: Register, value: Operand, metadata: u32) -> bool {
        let (seen, operand) = (
            self.seen(MetadataKind::UnaryArithmetic, metadata),
            self.get(value),
        );
        if !self.truncates_quietly(seen, &[operand]) {
            return false;
        }
        let bits = self.truncate(operand);
        let bits = self.ir().xor(bits, -1);
        let result = self.box_int32(bits);
        self.set(dst, result);
        true
    }

    pub(super) fn number_unary(
        &mut self,
        kind: Kind,
        dst: Register,
        value: Operand,
        metadata: u32,
    ) -> bool {
        let (seen, operand) = (
            self.seen(MetadataKind::UnaryArithmetic, metadata),
            self.get(value),
        );
        if kind == Kind::NumberToNumber && self.is(operand, T::BOXED_NUMBER) {
            self.set(dst, operand);
            return true;
        }
        let is_possible = self.may_be_numbers(seen, &[operand]);
        if is_possible {
            let result = self.number_node(kind, &[operand], seen);
            self.set(dst, result);
        }
        is_possible
    }

    /// `old` gets the number from before.
    pub(super) fn step(
        &mut self,
        kind: Kind,
        dst: Register,
        value: Register,
        old: Option<Register>,
        metadata: u32,
    ) -> bool {
        let (seen, operand) = (
            self.seen(MetadataKind::UnaryArithmetic, metadata),
            self.get(value),
        );
        if !self.may_be_numbers(seen, &[operand]) {
            return false;
        }
        let one = self.value(Value::int32(1));
        let result = self.number_node(kind, &[operand, one], seen);
        if let Some(old) = old {
            self.set(old, operand);
        }
        self.set(dst, result);
        true
    }

    // Comparisons give an I32, or `None` if the interpreter has to do it.

    fn may_convert_quietly(&self, operands: [Ssa; 2]) -> bool {
        !self.has_exited(ExitKind::ExoticObject)
            && !operands
                .iter()
                .any(|&operand| self.expected(operand).intersects(T::OBJECT))
    }

    pub(super) fn compare(
        &mut self,
        relation: Relation,
        lhs: Operand,
        rhs: Operand,
    ) -> Option<Ssa> {
        let operands = [self.get(lhs), self.get(rhs)].map(|operand| self.graph.root(operand));
        self.may_convert_quietly(operands)
            .then(|| self.node(Kind::Compare, &operands, relation as u32))
    }

    pub(super) fn loose_equal(&mut self, lhs: Operand, rhs: Operand) -> Option<Ssa> {
        let operands = [self.get(lhs), self.get(rhs)].map(|operand| self.graph.root(operand));
        let are_objects = operands
            .iter()
            .all(|&operand| (T::OBJECT | T::NULLISH).contains(self.expected(operand)));
        let is_quiet = self.may_convert_quietly(operands)
            || are_objects && !self.has_exited(ExitKind::ExoticObject);
        is_quiet.then(|| self.node(Kind::LooseEqual, &operands, 0))
    }

    pub(super) fn strict_equal(&mut self, lhs: Operand, rhs: Operand) -> Ssa {
        let operands = [self.get(lhs), self.get(rhs)].map(|operand| self.graph.root(operand));
        self.node(Kind::StrictEqual, &operands, 0)
    }

    pub(super) fn identical(&mut self, lhs: Operand, rhs: Operand) -> Ssa {
        let [lhs, rhs] = [self.get(lhs), self.get(rhs)].map(|operand| self.graph.root(operand));
        if lhs == rhs {
            self.int32(1)
        } else {
            self.ir().equal(lhs, rhs)
        }
    }

    pub(super) fn unsigned_compare(
        &mut self,
        cond: bun_engine_asm::Cond,
        lhs: Operand,
        rhs: Operand,
    ) -> Ssa {
        let [lhs, rhs] = [self.get(lhs), self.get(rhs)].map(|operand| self.as_int32(operand));
        self.ir().compare(cond, lhs, rhs)
    }

    pub(super) fn test(&mut self, kind: Kind, value: Operand, data: u32) -> Ssa {
        let value = self.get(value);
        let root = self.graph.root(value);
        self.node(kind, &[root], data)
    }

    pub(super) fn test_type(&mut self, value: Operand, ty: T) -> Ssa {
        let value = self.get(value);
        self.is_type(value, ty)
    }
}
