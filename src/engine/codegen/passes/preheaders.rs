//! The way into a natural loop.

use crate::analysis::{Cfg, Loop};
use crate::ir::{Builder, Edge, Function, Value};

/// They come before the edges from inside.
pub(super) fn entering_edges<'a>(function: &Function, cfg: &'a Cfg, entered: Loop) -> &'a [Edge] {
    let edges = cfg.predecessors(cfg.loops()[entered].header);
    &edges[..edges.partition_point(|&edge| !cfg.includes(entered, function.source(edge)))]
}

/// Makes the edges meet in a new block if there are several.
pub(super) fn only_entering_edge(function: &mut Function, entering: &[Edge]) -> Edge {
    if let [only] = *entering {
        return only;
    }
    let (header, meeting) = (function.target(entering[0]), function.new_block());
    let params: Vec<Value> = (0..function.params(header).len())
        .map(|position| {
            let ty = function.type_of(function.params(header)[position]);
            function.add_param(meeting, ty)
        })
        .collect();
    for &edge in entering {
        function.set_target(edge, meeting);
    }
    let origin = function.origin(function.first(header));
    let mut builder = Builder::at_end(function, meeting);
    builder.origin = origin;
    builder.jump(header, &params);
    function.successor(meeting, 0)
}
