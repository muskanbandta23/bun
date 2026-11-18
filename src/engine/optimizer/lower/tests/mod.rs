//! Graphs built by hand, lowered, and run as machine code.

mod harness;

use bun_engine_standalone as _;

use bun_engine_asm::{Features, abi};
use bun_engine_codegen::ir::{self, Binary, Boundary, Origin, State, Type, validate};
use bun_engine_codegen::passes::Level;
use bun_engine_vm::{ExitKinds, SpeculatedType as T, Value};

use super::Compiler;
use crate::graph::{
    CallKind, Entry, ExitKind, FrameIndex, FrameInfo, Graph, Kind, Layout, NO_LAYOUT, NO_RECIPE,
    Position, Site, TypeCheck,
};
use harness::Harness;

/// A graph of one frame with one instruction, whose registers a state can name.
fn empty_graph(parameters: u32) -> Graph {
    let mut graph = Graph::new(Level::Quick, Entry::Function);
    graph.frames.push(FrameInfo {
        code_block: 0,
        global: 0,
        caller: Origin::default(),
        kind: CallKind::Call,
        frame_words: 4,
        parameters,
        depth: 0,
    });
    graph.sites.push(Site {
        position: Position {
            frame: FrameIndex::ROOT,
            offset: 0,
        },
        exits: ExitKinds::default(),
        metadata: 0,
        has_handler: false,
    });
    graph.function.reserved_frame_bytes = 4 * 8;
    graph
}

/// A layout that names `count` locals, so that a state can be described.
fn plain_layout(graph: &mut Graph, count: u32) -> u32 {
    let first_register = graph.layout_registers.len() as u32;
    graph
        .layout_registers
        .extend((0..count).map(bun_engine_bytecode::Register::local));
    graph.layouts.push(Layout {
        resume: Origin::default(),
        caller: NO_LAYOUT,
        first_register,
        register_count: count,
        recipe: NO_RECIPE,
    });
    graph.layouts.len() as u32 - 1
}

fn entry_in_registers(graph: &mut Graph, count: usize) -> ir::Block {
    let params: Vec<_> = (0..count)
        .map(|index| {
            (
                Type::I64,
                Boundary::Register(abi::ARGUMENT_GPRS[index].into()),
            )
        })
        .collect();
    let block = graph.function.add_entry(&params);
    graph
        .function
        .set_results(&[Boundary::Register(abi::RETURN_GPRS[0].into())]);
    block
}

/// Adds two tagged values that have to be int32, and gives the tagged sum.
fn adding_graph() -> Graph {
    let mut graph = empty_graph(1);
    let block = entry_in_registers(&mut graph, 2);
    let [left, right] = [0, 1].map(|index| graph.function.params(block)[index]);
    let layout = plain_layout(&mut graph, 2);
    let state = graph.function.add_described_state([left, right], layout);
    let check = graph.type_check(TypeCheck {
        wanted: T::INT32,
        known: T::HEAP_TOP,
        failure: ExitKind::BadType,
    });
    let mut b = ir::Builder::at_end(&mut graph.function, block);
    let checked =
        [left, right].map(|value| b.node(Kind::CheckType.id(), Type::I64, &[value], check, state));
    let narrow = checked.map(|value| b.wrap(value));
    let sum = b.binary(Binary::Add, narrow[0], narrow[1]);
    let boxed = b.node(Kind::BoxInt32.id(), Type::I64, &[sum], 0, State::NONE);
    b.ret(&[boxed]);
    graph
}

#[test]
fn lowering_leaves_no_node() {
    let mut graph = adding_graph();
    let mut sites = super::Sites::default();
    super::run(&mut graph, &mut sites).unwrap();
    validate(&graph.function).unwrap();
    let nodes = graph
        .function
        .blocks()
        .flat_map(|block| graph.function.instructions(block))
        .filter(|&value| graph.function.op(value).node().is_some())
        .count();
    assert_eq!(nodes, 0);
}

#[test]
fn adds_two_int32s() {
    let output = Compiler::default()
        .compile(adding_graph(), Features::host())
        .unwrap();
    let mut harness = Harness::new(&output);
    let mut add =
        |a: i32, b: i32| harness.call(&[Value::int32(a).to_bits(), Value::int32(b).to_bits()]);
    assert_eq!(add(2, 3), Value::int32(5).to_bits());
    assert_eq!(add(-1, 1), Value::int32(0).to_bits());
    assert_eq!(add(i32::MAX, 1), Value::int32(i32::MIN).to_bits());
}

#[test]
fn leaves_for_a_value_that_is_no_int32() {
    let output = Compiler::default()
        .compile(adding_graph(), Features::host())
        .unwrap();
    assert_eq!(output.exits.len(), 2);
    assert!(
        output
            .exits
            .iter()
            .all(|exit| exit.kind == ExitKind::BadType)
    );
    assert!(output.exits.iter().all(|exit| exit.value_count == 2));
    let mut harness = Harness::new(&output);
    let taken =
        harness.call_expecting_exit(&[Value::UNDEFINED.to_bits(), Value::int32(1).to_bits()]);
    assert_eq!(taken, 0);
    let taken =
        harness.call_expecting_exit(&[Value::int32(1).to_bits(), Value::UNDEFINED.to_bits()]);
    assert_eq!(taken, 1);
}
