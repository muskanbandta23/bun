use bun_engine_asm::{Cond, FloatCond};
use bun_engine_codegen::ir::Value as Ssa;
use bun_engine_vm::{RealmWatchpoint, SpeculatedType as T};

use super::Specializer;
use crate::graph::{ExitKind, Kind, Relation, TypeCheck};

/// Equal to nothing but the value with the same bits.
const BY_IDENTITY: T = T::OBJECT
    .union(T::SYMBOL)
    .union(T::OTHER_CELL)
    .union(T::BOOLEAN)
    .union(T::NULLISH)
    .union(T::EMPTY);
const INTEGER: T = T::INT32.union(T::INT52);
const QUIET: T = T::FULL_NUMBER.union(T::BOOLEAN).union(T::NULLISH);

impl Specializer<'_> {
    fn both_expected(&self, lhs: Ssa, rhs: Ssa, ty: T) -> bool {
        let are_known = self.is(lhs, ty) && self.is(rhs, ty);
        are_known
            || ty.contains(self.expected(lhs) | self.expected(rhs))
                && !self.has_exited(ExitKind::BadType)
    }

    fn compare_numbers(
        &mut self,
        cond: Cond,
        float_cond: FloatCond,
        lhs: Ssa,
        rhs: Ssa,
    ) -> Option<Ssa> {
        if self.both_expected(lhs, rhs, T::INT32) {
            let [lhs, rhs] = [lhs, rhs].map(|operand| self.as_int32(operand, ExitKind::BadType));
            Some(self.ir().compare(cond, lhs, rhs))
        } else if self.both_expected(lhs, rhs, INTEGER) {
            let [lhs, rhs] = [lhs, rhs].map(|operand| self.as_int52(operand, ExitKind::BadType));
            Some(self.ir().compare(cond, lhs, rhs))
        } else if self.both_expected(lhs, rhs, T::FULL_NUMBER) {
            let [lhs, rhs] = [lhs, rhs].map(|operand| self.as_double(operand, ExitKind::BadType));
            Some(self.ir().float_compare(float_cond, lhs, rhs))
        } else {
            None
        }
    }

    fn checked_both(&mut self, lhs: Ssa, rhs: Ssa, ty: T, failure: ExitKind) -> [Ssa; 2] {
        [lhs, rhs].map(|operand| self.check_type(operand, ty, failure))
    }

    pub(super) fn compare(&mut self, relation: Relation, lhs: Ssa, rhs: Ssa) -> Ssa {
        let (cond, float_cond) = match relation {
            Relation::Less => (Cond::Lt, FloatCond::Lt),
            Relation::LessEqual => (Cond::Le, FloatCond::Le),
            Relation::Greater => (Cond::Gt, FloatCond::Gt),
            Relation::GreaterEqual => (Cond::Ge, FloatCond::Ge),
        };
        if let Some(result) = self.compare_numbers(cond, float_cond, lhs, rhs) {
            return result;
        }
        if self.both_expected(lhs, rhs, QUIET) {
            let [lhs, rhs] = [lhs, rhs].map(|operand| self.to_double(operand));
            return self.ir().float_compare(float_cond, lhs, rhs);
        }
        let operands = self.checked_both(lhs, rhs, T::HEAP_TOP - T::OBJECT, ExitKind::ExoticObject);
        self.node(Kind::PrimitiveCompare, &operands, relation as u32)
    }

    fn same_bits(&mut self, lhs: Ssa, rhs: Ssa) -> Ssa {
        let [lhs, rhs] = [lhs, rhs].map(|operand| self.graph.root(operand));
        self.ir().equal(lhs, rhs)
    }

    fn equal_of_one_type(&mut self, lhs: Ssa, rhs: Ssa) -> Option<Ssa> {
        if let Some(result) = self.compare_numbers(Cond::Eq, FloatCond::Eq, lhs, rhs) {
            return Some(result);
        }
        if self.both_expected(lhs, rhs, T::ATOM_STRING) {
            self.checked_both(lhs, rhs, T::ATOM_STRING, ExitKind::BadType);
            let operands = [self.alias(lhs), self.alias(rhs)];
            return Some(self.node(Kind::AtomEqual, &operands, 0));
        }
        if self.both_expected(lhs, rhs, T::STRING) {
            let operands = self.checked_both(lhs, rhs, T::STRING, ExitKind::BadType);
            return Some(self.node(Kind::StringEqual, &operands, 0));
        }
        None
    }

    pub(super) fn strict_equal(&mut self, lhs: Ssa, rhs: Ssa) -> Ssa {
        if self.is(lhs, BY_IDENTITY) || self.is(rhs, BY_IDENTITY) {
            return self.same_bits(lhs, rhs);
        }
        if let Some(result) = self.equal_of_one_type(lhs, rhs) {
            return result;
        }
        if !self.has_exited(ExitKind::BadType) {
            for operand in [lhs, rhs] {
                for ty in [
                    T::OBJECT,
                    T::BOOLEAN,
                    T::OBJECT | T::NULLISH,
                    T::BOOLEAN | T::NULLISH,
                    T::SYMBOL,
                ] {
                    if ty.contains(self.expected(operand)) {
                        self.check_type(operand, ty, ExitKind::BadType);
                        return self.same_bits(lhs, rhs);
                    }
                }
            }
        }
        self.node(Kind::GenericStrictEqual, &[lhs, rhs], 0)
    }

    pub(super) fn loose_equal(&mut self, lhs: Ssa, rhs: Ssa) -> Ssa {
        if let Some(result) = self.equal_of_one_type(lhs, rhs) {
            return result;
        }
        for ty in [T::OBJECT, T::BOOLEAN, T::SYMBOL] {
            if self.both_expected(lhs, rhs, ty) {
                self.checked_both(lhs, rhs, ty, ExitKind::BadType);
                return self.same_bits(lhs, rhs);
            }
        }
        let expected = self.expected(lhs) | self.expected(rhs);
        let quiet = if expected.intersects(T::OBJECT) {
            T::OBJECT | T::NULLISH
        } else {
            T::HEAP_TOP - T::OBJECT
        };
        let operands = self.checked_both(lhs, rhs, quiet, ExitKind::ExoticObject);
        self.node(Kind::QuietLooseEqual, &operands, 0)
    }

    fn objects_are_what_they_seem(&mut self) -> bool {
        self.graph
            .try_to_assume(self.origin, RealmWatchpoint::NoObjectMasqueradesAsUndefined)
    }

    fn is_type(&mut self, value: Ssa, wanted: T) -> Ssa {
        let known = self.type_of(value);
        if wanted.contains(known) || !wanted.intersects(known) {
            return self.graph.function.const32(wanted.contains(known).into());
        }
        let data = self.graph.type_check(TypeCheck {
            wanted,
            known,
            failure: ExitKind::BadType,
        });
        self.node(Kind::IsType, &[value], data)
    }

    pub(super) fn equal_null(&mut self, value: Ssa) -> Ssa {
        if !self.type_of(value).intersects(T::OBJECT) || self.objects_are_what_they_seem() {
            self.is_type(value, T::NULLISH)
        } else {
            self.node(Kind::GenericEqualNull, &[value], 0)
        }
    }

    pub(super) fn to_boolean(&mut self, value: Ssa) -> Ssa {
        let expects = |ty: T| {
            self.is(value, ty)
                || ty.contains(self.expected(value)) && !self.has_exited(ExitKind::BadType)
        };
        if expects(T::BOOLEAN) {
            self.as_boolean(value, ExitKind::BadType)
        } else if expects(T::INT32) {
            let int32 = self.as_int32(value, ExitKind::BadType);
            self.ir().is_not_zero(int32)
        } else if expects(T::FULL_NUMBER) {
            let double = self.as_double(value, ExitKind::BadType);
            let zero = self.graph.function.const_f64(0.0);
            self.ir().float_compare(FloatCond::Ne, double, zero)
        } else if expects(T::STRING) {
            let string = self.check_type(value, T::STRING, ExitKind::BadType);
            let length = self.node(Kind::StringLength, &[string], 0);
            self.ir().is_not_zero(length)
        } else if expects(T::BOOLEAN | T::NULLISH) {
            self.check_type(value, T::BOOLEAN | T::NULLISH, ExitKind::BadType);
            let root = self.graph.root(value);
            self.ir().equal(root, bun_engine_vm::Value::TRUE.to_bits())
        } else if expects(T::OBJECT | T::NULLISH) && self.objects_are_what_they_seem() {
            self.check_type(value, T::OBJECT | T::NULLISH, ExitKind::BadType);
            self.is_type(value, T::OBJECT)
        } else {
            self.node(Kind::GenericToBoolean, &[value], 0)
        }
    }
}
