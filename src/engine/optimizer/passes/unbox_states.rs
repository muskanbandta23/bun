//! Leaves it to the exit to put tags on values.

use bun_engine_codegen::ir::{Entity, State};
use bun_engine_codegen::passes::Analyses;

use crate::graph::{Format, Graph, Kind};

pub(super) fn run(graph: &mut Graph, analyses: &mut Analyses) {
    for &block in analyses.cfg(&mut graph.function).order() {
        let checkpoint = graph
            .function
            .instructions(block)
            .find(|&value| graph.kind(value) != Some(Kind::Refine) && !is_box(graph.kind(value)));
        if let Some(checkpoint) =
            checkpoint.filter(|&value| graph.kind(value) == Some(Kind::Checkpoint))
        {
            graph.function.remove(checkpoint);
        }
    }
    for state in (0..graph.function.state_count()).map(State::from_index) {
        for position in 0..graph.function.state(state).len() {
            let value = graph.function.state(state)[position];
            let format = match graph.kind(value) {
                Some(Kind::BoxInt32) => Format::Int32,
                Some(Kind::BoxInt52) => Format::Int52,
                Some(Kind::BoxDouble) => Format::Double,
                Some(Kind::BoxBoolean) => Format::Boolean,
                _ => continue,
            };
            graph
                .function
                .retag(state, position, graph.operand(value, 0), format as u8);
        }
    }
}

fn is_box(kind: Option<Kind>) -> bool {
    matches!(
        kind,
        Some(Kind::BoxInt32 | Kind::BoxInt52 | Kind::BoxDouble | Kind::BoxBoolean)
    )
}
