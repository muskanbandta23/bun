//! What a graph of this engine must hold beyond what the back end checks. Debug builds run it after every pass.

use bun_engine_codegen::ir::{EffectFlags, Entity, Origin, Value as Ssa};

use super::heaps::{STRING_FIBER, WORLD};
use super::{Gives, Graph, Kind};

/// Describes the first thing that is wrong, so that a pass that breaks the graph names itself.
pub(crate) fn validate(graph: &Graph) -> Result<(), String> {
    let function = &graph.function;
    for block in function.blocks() {
        if function.is_removed(block) {
            continue;
        }
        // An instruction is re-run from its start after an exit, so what it has already written would be written twice.
        let mut written: Vec<Origin> = Vec::new();
        for value in function.instructions(block) {
            let Some(kind) = graph.kind(value) else {
                continue;
            };
            let origin = function.origin(value);
            if origin.0 as usize >= graph.sites.len() {
                return Err(at(graph, value, format_args!("origin {}", origin.0)));
            }
            let effects = kind.effects();
            let exits = effects.has(EffectFlags::EXITS);
            if exits != !function.state_of(value).is_none() {
                let with = if exits { "no state" } else { "a state" };
                return Err(at(graph, value, format_args!("{with}")));
            }
            if exits && written.contains(&origin) {
                return Err(at(graph, value, format_args!("exits after it has written")));
            }
            if function.may_alias(effects.writes, WORLD) && effects.writes != STRING_FIBER {
                written.push(origin);
            }
            if let Err(problem) = gives(graph, value, kind) {
                return Err(at(graph, value, format_args!("{problem}")));
            }
        }
    }
    Ok(())
}

/// The machine type of a node is the one its kind gives.
fn gives(graph: &Graph, value: Ssa, kind: Kind) -> Result<(), String> {
    let (function, wanted) = (&graph.function, kind.gives().machine_type());
    let ty = function.type_of(value);
    if ty != wanted {
        return Err(format!("is {ty:?}, not {wanted:?}"));
    }
    if matches!(kind.gives(), Gives::Operand) {
        let inside = graph.operand(value, 0);
        if inside == value {
            return Err("gives itself".into());
        }
        if function.type_of(inside) != ty {
            return Err(format!("gives {:?}", function.type_of(inside)));
        }
    }
    Ok(())
}

fn at(graph: &Graph, value: Ssa, problem: core::fmt::Arguments<'_>) -> String {
    let name = graph.kind(value).map_or("", |kind| kind.name());
    format!("v{} {name} {problem}", value.index())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the table of effects must say of itself, so that nothing has to read it twice.
    #[test]
    fn effects_agree() {
        for &kind in Kind::all() {
            let (effects, name) = (kind.effects(), kind.name());
            assert!(
                !kind.can_throw() || effects.has(EffectFlags::EXITS),
                "{name} throws without leaving"
            );
            assert!(
                !kind.can_throw() || kind.can_collect(),
                "{name} throws without collecting"
            );
            assert!(
                effects.writes != WORLD || effects.has(EffectFlags::EXITS),
                "{name} writes everything without leaving"
            );
            assert!(
                !matches!(kind.gives(), Gives::Tuple(elements) if elements.is_empty()),
                "{name} gives an empty tuple"
            );
        }
    }
}
