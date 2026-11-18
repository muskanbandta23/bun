//! Carries what profiles have seen forward to the values that have none.

use bun_engine_codegen::ir::Value as Ssa;
use bun_engine_codegen::passes::Analyses;
use bun_engine_vm::SpeculatedType as T;

use super::usage::{Usage, Use};
use crate::graph::{ArithmeticData as Seen, Graph, Kind};

const INTEGER: T = T::INT32.union(T::INT52);

fn fits_int32(seen: Seen, usage: Use) -> bool {
    (!seen.intersects(Seen::MAY_OVERFLOW_INT32 | Seen::MAY_BE_DOUBLE)
        || !usage.contains(Use::AS_NUMBER))
        && (!seen.contains(Seen::MAY_BE_NEGATIVE_ZERO) || !usage.contains(Use::SIGN_OF_ZERO))
}

fn fits_int52(seen: Seen, usage: Use) -> bool {
    !seen.contains(Seen::MAY_OVERFLOW_INT52)
        && (!seen.contains(Seen::MAY_BE_NEGATIVE_ZERO) || !usage.contains(Use::SIGN_OF_ZERO))
}

fn predict(graph: &Graph, value: Ssa, kind: Kind, data: u32, usage: Use) -> Option<T> {
    let seen = Seen::from_bits_retain(data);
    let operands = match kind {
        Kind::UInt32ToNumber => {
            return Some(if fits_int32(seen, usage) {
                T::INT32
            } else {
                INTEGER
            });
        }
        Kind::NumberAdd
        | Kind::NumberSubtract
        | Kind::NumberMultiply
        | Kind::NumberDivide
        | Kind::NumberRemainder
        | Kind::NumberNegate
        | Kind::NumberToNumber => graph
            .function
            .operands(value)
            .iter()
            .map(|&operand| graph.prediction(operand)),
        _ => return None,
    };
    if operands.clone().any(|prediction| prediction.is_empty()) {
        return None;
    }
    let all = operands.fold(T::empty(), |all, prediction| all | prediction);
    let are_int32 = T::INT32.contains(all) && !seen.contains(Seen::SAW_DOUBLE);
    let are_integers =
        INTEGER.contains(all) && (all.intersects(T::INT52) || !seen.contains(Seen::SAW_DOUBLE));
    Some(match kind {
        Kind::NumberToNumber if seen.contains(Seen::SAW_DOUBLE) => {
            all & T::FULL_NUMBER | T::BOXED_NUMBER
        }
        Kind::NumberToNumber => all & T::FULL_NUMBER,
        // A product can be too large to be exact, so its low bits are not those of the integer.
        Kind::NumberMultiply if are_int32 && fits_int32(seen, usage | Use::AS_NUMBER) => T::INT32,
        Kind::NumberMultiply if are_integers && fits_int52(seen, usage) => INTEGER,
        Kind::NumberMultiply => T::BOXED_DOUBLE,
        _ if are_int32 && fits_int32(seen, usage) => T::INT32,
        Kind::NumberAdd | Kind::NumberSubtract | Kind::NumberNegate
            if are_integers && fits_int52(seen, usage) =>
        {
            INTEGER
        }
        _ => T::BOXED_DOUBLE,
    })
}

pub(super) fn run(graph: &mut Graph, analyses: &mut Analyses, usage: &Usage) {
    let cfg = analyses.cfg(&mut graph.function);
    graph
        .predictions
        .grow_to(graph.function.value_count(), T::empty());
    let has_cycles = cfg.loops().len() != 0;
    let mut changed = true;
    while core::mem::take(&mut changed) {
        for &block in cfg.order() {
            for &edge in cfg.predecessors(block) {
                for position in 0..graph.function.args(edge).len() {
                    let (arg, param) = (
                        graph.function.args(edge)[position],
                        graph.function.params(block)[position],
                    );
                    let widened = graph.predictions[param] | graph.prediction(arg);
                    changed |= core::mem::replace(&mut graph.predictions[param], widened)
                        != widened
                        && has_cycles;
                }
            }
            let mut value = graph.function.first(block);
            while !value.is_none() {
                if let Some((kind, data)) = graph.node(value)
                    && let Some(prediction) = predict(graph, value, kind, data, usage.of(value))
                {
                    let widened = graph.predictions[value] | prediction;
                    changed |= core::mem::replace(&mut graph.predictions[value], widened)
                        != widened
                        && has_cycles;
                }
                value = graph.function.next(value);
            }
        }
    }
}
