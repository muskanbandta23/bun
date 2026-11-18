use bun_engine_asm::{Cond, Sign};
use bun_engine_codegen::ir::{Type, Unary, Value as Ssa};
use bun_engine_vm::{SpeculatedType as T, Value};

use super::Specializer;
use crate::graph::{ExitKind, Kind};
use crate::passes::unbox_params::ON_EDGE;
use crate::passes::usage::Use;

impl Specializer<'_> {
    pub(super) fn conversion_failure(&self, data: u32) -> ExitKind {
        if data == ON_EDGE {
            ExitKind::BadRepresentation
        } else {
            ExitKind::BadType
        }
    }

    pub(super) fn constant_number(&self, value: Ssa) -> Option<f64> {
        let bits = self.graph.constant(value)?;
        if bits >= Value::NUMBER_TAG {
            Some((bits as i32).into())
        } else {
            (bits >= Value::DOUBLE_OFFSET).then(|| f64::from_bits(bits - Value::DOUBLE_OFFSET))
        }
    }

    pub(super) fn boxed(&mut self, kind: Kind, unboxed: Ssa) -> Ssa {
        let boxed = self.node(kind, &[unboxed], 0);
        self.graph.predict(boxed, self.graph.prediction(self.at));
        boxed
    }

    fn int_to_double(&mut self, int: Ssa) -> Ssa {
        self.ir()
            .convert(Unary::IntToFloat(Sign::Signed), Type::F64, int)
    }

    pub(super) fn as_int32(&mut self, value: Ssa, failure: ExitKind) -> Ssa {
        let root = self.graph.root(value);
        if let Some(known) = self
            .knowledge
            .get(root)
            .and_then(|known| known.int32.expand())
        {
            return known;
        }
        let exact = self
            .constant_number(root)
            .filter(|&number| f64::from(number as i32).to_bits() == number.to_bits());
        if let Some(number) = exact {
            return self.graph.function.const32(number as i32);
        }
        let int32 = match self.graph.kind(root) {
            Some(Kind::BoxInt32) => return self.graph.operand(root, 0),
            Some(Kind::BoxInt52) => {
                let wide = self.graph.operand(root, 0);
                let mut ir = self.ir();
                let narrow = ir.wrap(wide);
                let back = ir.sign_extend(narrow);
                let differs = ir.not_equal(back, wide);
                self.exit_if(differs, failure);
                narrow
            }
            Some(Kind::BoxDouble) => {
                self.node(Kind::DoubleAsInt32, &[self.graph.operand(root, 0)], 0)
            }
            _ if !T::INT32.contains(self.expected(root))
                && T::BOXED_NUMBER.contains(self.expected(root)) =>
            {
                let double = self.as_double(root, failure);
                return self.node(Kind::DoubleAsInt32, &[double], 0);
            }
            _ => {
                let checked = self.check_type(root, T::INT32, failure);
                self.ir().wrap(checked)
            }
        };
        self.knowledge.entry(self.graph, root).int32 = int32;
        int32
    }

    pub(super) fn as_int52(&mut self, value: Ssa, failure: ExitKind) -> Ssa {
        let root = self.graph.root(value);
        if let Some(number) = self
            .constant_number(root)
            .and_then(bun_engine_number::exact_int52)
        {
            return self.graph.function.const64(number);
        }
        match self.graph.kind(root) {
            Some(Kind::BoxInt52) => self.graph.operand(root, 0),
            _ if T::INT32.contains(self.expected(root)) => {
                let int32 = self.as_int32(root, failure);
                self.ir().sign_extend(int32)
            }
            _ => {
                let double = self.as_double(root, failure);
                self.node(Kind::DoubleAsInt52, &[double], 0)
            }
        }
    }

    pub(super) fn as_double(&mut self, value: Ssa, failure: ExitKind) -> Ssa {
        let root = self.graph.root(value);
        if let Some(known) = self
            .knowledge
            .get(root)
            .and_then(|known| known.double.expand())
        {
            return known;
        }
        if let Some(number) = self.constant_number(root) {
            return self.graph.function.const_f64(number);
        }
        let double = match self.graph.kind(root) {
            Some(Kind::BoxDouble) => return self.graph.operand(root, 0),
            Some(Kind::BoxInt32 | Kind::BoxInt52) => {
                self.int_to_double(self.graph.operand(root, 0))
            }
            _ if self.is(root, T::INT32) => {
                let int32 = self.as_int32(root, failure);
                self.int_to_double(int32)
            }
            _ => {
                let number = self.check_type(root, T::BOXED_NUMBER, failure);
                let is_mostly_int32 = T::INT32.contains(self.expected(root));
                self.node(Kind::NumberToDouble, &[number], is_mostly_int32.into())
            }
        };
        self.knowledge.entry(self.graph, root).double = double;
        double
    }

    pub(super) fn as_boolean(&mut self, value: Ssa, failure: ExitKind) -> Ssa {
        let root = self.graph.root(value);
        self.graph
            .inside(root, Kind::BoxBoolean)
            .unwrap_or_else(|| {
                let checked = self.check_type(root, T::BOOLEAN, failure);
                let mut ir = self.ir();
                let low = ir.wrap(checked);
                ir.and(low, 1)
            })
    }

    /// ToNumber of what converts without running scripts or reading text.
    pub(super) fn to_double(&mut self, value: Ssa) -> Ssa {
        if T::FULL_NUMBER.contains(self.expected(value)) && !self.has_exited(ExitKind::BadType) {
            return self.as_double(value, ExitKind::BadType);
        }
        let primitive = self.check_type(
            value,
            T::BOXED_NUMBER | T::BOOLEAN | T::NULLISH,
            ExitKind::ExoticObject,
        );
        self.node(Kind::PrimitiveToDouble, &[primitive], 0)
    }

    pub(super) fn truncate(&mut self, value: Ssa) -> Ssa {
        let (root, may_speculate) = (self.graph.root(value), !self.has_exited(ExitKind::BadType));
        if let Some(number) = self.constant_number(root) {
            return self
                .graph
                .function
                .const32(bun_engine_number::to_int32(number));
        }
        if let Some(wide) = self.graph.inside(root, Kind::BoxInt52) {
            return self.ir().wrap(wide);
        }
        if self.is(root, T::INT32) || may_speculate && T::INT32.contains(self.expected(root)) {
            return self.as_int32(root, ExitKind::BadType);
        }
        if self.is(root, T::BOOLEAN) || may_speculate && T::BOOLEAN.contains(self.expected(root)) {
            return self.as_boolean(root, ExitKind::BadType);
        }
        let double = self.to_double(root);
        self.node(Kind::TruncateDouble, &[double], 0)
    }

    pub(super) fn uint32_to_number(&mut self, bits: Ssa) -> Ssa {
        if !T::INT32.contains(self.graph.prediction(self.at)) {
            let wide = self.ir().zero_extend(bits);
            return self.boxed(Kind::BoxInt52, wide);
        }
        if self.usage.of(self.at).contains(Use::AS_NUMBER) {
            let is_large = self.ir().compare(Cond::Lt, bits, 0);
            self.exit_if(is_large, ExitKind::Overflow);
        }
        self.boxed(Kind::BoxInt32, bits)
    }
}
