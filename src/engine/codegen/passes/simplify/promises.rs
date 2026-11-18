//! A load that is `ALWAYS_READABLE` may count on its address being computed by what cannot move above a check.

use super::Simplifier;
use super::numbering::is_expression;
use crate::ir::{AccessFlags, EffectFlags, Entity, Op, Value};
use crate::passes::readability::withdraw_promise;

#[inline]
pub(super) fn makes_promise(op: &Op) -> bool {
    matches!(op, Op::Load { access, .. } | Op::LoadLane { access, .. }
        if access.has(AccessFlags::ALWAYS_READABLE))
}

impl Simplifier<'_> {
    /// Before the value is computed in another way.
    #[inline]
    pub(super) fn set_free(&mut self, value: Value) {
        const HELD: EffectFlags = EffectFlags::EXITS.union(EffectFlags::CONTROL_DEPENDENT);
        if self.has_promises && self.function.effects(value).has(HELD) {
            (self.tables.values[value].is_set_free, self.has_set_free) = (true, true);
        }
    }

    /// Before the uses of `user` are resolved.
    pub(super) fn follow_what_is_set_free(&mut self, user: Value) {
        let (function, values) = (&mut *self.function, &mut self.tables.values);
        if function
            .operands(user)
            .iter()
            .any(|&operand| values[operand].is_set_free)
        {
            values[user].is_set_free = is_expression(function.op(user));
            withdraw_promise(function, user);
        }
        if user != function.last(self.block) {
            return;
        }
        for edge in function.successors(self.block) {
            let target = function.target(edge);
            for (&param, &arg) in function.params(target).iter().zip(function.args(edge)) {
                if values[arg].is_set_free && !values[param].is_set_free {
                    values[param].is_set_free = true;
                    self.withdraws_all_promises |=
                        self.cfg.position(target) <= self.cfg.position(self.block);
                }
            }
        }
    }

    pub(super) fn keep_promises(&mut self) {
        if self.withdraws_all_promises {
            self.hit();
            for value in (0..self.function.value_count()).map(Value::from_index) {
                if makes_promise(self.function.op(value)) {
                    withdraw_promise(self.function, value);
                }
            }
        }
    }
}
