//! From the graph to machine code: what stands for run-time work becomes plain instructions and patchpoints.

pub(crate) mod output;

use bun_engine_asm::Features;

use crate::builder::{Built, Refusal};
use crate::graph::Graph;
use output::Output;

/// Instruction selection, register allocation and the tables that describe the code. It reads no heap.
pub(crate) fn compile(graph: Graph, features: Features) -> Built<Output> {
    let _ = (graph, features);
    Err(Refusal::Unsupported)
}
