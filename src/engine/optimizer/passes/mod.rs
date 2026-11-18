//! What is done to a graph away from the heap.

mod predict;
mod specialize;
mod unbox_params;
mod unbox_states;
mod usage;

use bun_engine_codegen::Target;
use bun_engine_codegen::ir::validate as validate_machine;
use bun_engine_codegen::passes::{Analyses, Pass};

use crate::graph::{Graph, validate};

fn checked(graph: &Graph, name: &str) {
    if !cfg!(debug_assertions) {
        return;
    }
    let found = validate_machine(&graph.function).and_then(|()| validate(graph));
    if let Err(problem) = found {
        panic!("after {name}: {problem}\n{:?}", graph.function);
    }
}

pub(crate) fn optimize(
    graph: &mut Graph,
    analyses: &mut Analyses,
    mut observe: impl FnMut(&str, &Graph),
) {
    let target = Target::host();
    let mut done = |graph: &Graph, name: &str| {
        checked(graph, name);
        observe(name, graph);
    };
    analyses.invalidate();
    analyses.cfg(&mut graph.function);
    done(graph, "build");
    let usage = usage::run(graph, analyses);
    predict::run(graph, analyses, &usage);
    unbox_params::run(graph, analyses);
    done(graph, "unbox_params");
    specialize::run(graph, analyses, &usage);
    done(graph, "specialize");
    unbox_states::run(graph, analyses);
    Pass::named("eliminate_dead_code").run(&mut graph.function, analyses, target);
    done(graph, "eliminate_dead_code");
}
