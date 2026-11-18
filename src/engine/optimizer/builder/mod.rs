//! From bytecode and profiles to a graph. This is the part that reads the heap.

mod arithmetic;
mod calls;
mod cells;
mod control;
mod elements;
mod emit;
mod frame;
mod instructions;
mod interpret;
mod objects;
mod properties;
mod scopes;
mod states;

use bun_engine_asm::abi;
use bun_engine_bytecode::{Register, Walk};
use bun_engine_codegen::ir::{Block, Boundary, Heap, Origin, State, Type, Value as Ssa};
use bun_engine_codegen::passes::Level;
use bun_engine_vm::{CodeBlock, FastHashMap, Gc, RootVec, ShapeId, Vm, root_vec};
pub(crate) use cells::Dependencies;
use frame::{ArgumentCount, Frame};

use crate::graph::{CallKind, Entry, FrameInfo, Graph, Knowledge, ShapeIndex, heaps};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Refusal {
    TooLarge,
    Unsupported,
}

pub(crate) type Built<T = ()> = Result<T, Refusal>;

pub(crate) struct Builder<'a> {
    vm: &'a Vm,
    graph: Graph,
    dependencies: Dependencies,
    /// The innermost is last.
    frames: RootVec<Frame>,
    /// `NONE` where control does not get.
    block: Block,
    origin: Origin,
    knowledge: Knowledge,
    /// Where to exit to, as long as nothing has happened that cannot be done again.
    checkpoint: State,
    /// The instruction that is being translated has had an effect, so there is nothing to exit to until the next.
    has_had_effect: bool,
    needs_invalidation_point: bool,
    shape_indices: FastHashMap<ShapeId, ShapeIndex>,
    heap_members: FastHashMap<(heaps::Family, usize), Heap>,
    inlined_size: u32,
    walk: Walk,
    scratch: Vec<Ssa>,
}

pub(crate) fn build(
    vm: &Vm,
    code_block: Gc<CodeBlock>,
    level: Level,
    entry: Entry,
) -> Built<(Graph, Dependencies)> {
    let _defer = vm.heap.defer_collection();
    let mut builder = Builder {
        vm,
        graph: Graph::new(level, entry),
        dependencies: Dependencies::default(),
        frames: root_vec(),
        block: Block::NONE,
        origin: Origin::default(),
        knowledge: Knowledge::default(),
        checkpoint: State::NONE,
        has_had_effect: false,
        needs_invalidation_point: false,
        shape_indices: FastHashMap::default(),
        heap_members: FastHashMap::default(),
        inlined_size: 0,
        walk: Walk::default(),
        scratch: Vec::new(),
    };
    builder.build_root(code_block)?;
    builder.graph.function.resolve_aliases();
    Ok((builder.graph, builder.dependencies))
}

impl Builder<'_> {
    #[inline]
    fn frame(&self) -> &Frame {
        self.frames.last().unwrap()
    }

    #[inline]
    fn frame_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().unwrap()
    }

    fn is_full(&self) -> bool {
        self.graph.level == Level::Full
    }

    fn describe_frame(&mut self, code_block: Gc<CodeBlock>, kind: CallKind) -> FrameInfo {
        code_block.update_predictions();
        self.dependencies.keep(code_block.cell());
        self.note_realm(code_block.global());
        FrameInfo {
            code_block: code_block.address(),
            global: code_block.global().address(),
            caller: self.origin,
            kind,
            frame_words: code_block.locals(),
            parameters: code_block.parameters(),
            depth: self.frames.len() as u32,
        }
    }

    fn build_root(&mut self, code_block: Gc<CodeBlock>) -> Built {
        let info = self.describe_frame(code_block, CallKind::Call);
        let mut frame = Frame::new(
            &mut self.graph,
            info,
            code_block,
            info.parameters,
            ArgumentCount::InFrame,
        )?;
        let start = match self.graph.entry {
            Entry::Function => 0,
            Entry::Loop(offset) => offset,
        };
        let first = frame.liveness.graph().block_at(start);

        let mut registers = vec![Register::CALLEE];
        registers.extend(
            (0..info.parameters)
                .map(|index| Register::from_offset(Register::THIS.offset() + index as i32)),
        );
        if let Entry::Loop(_) = self.graph.entry {
            registers.extend(frame.liveness.live_in(first).iter());
        }
        // The slots of the bytecode frame stay where the lower tiers put them, so that the interpreter can run an
        // instruction of this function on the frame itself, and an exit has less to move.
        self.graph.function.reserved_frame_bytes = info.frame_words * 8;
        let params: Vec<_> = registers
            .iter()
            .map(|register| (Type::I64, Boundary::Frame(register.offset() * 8)))
            .collect();
        self.block = self.graph.function.add_entry(&params);
        self.graph
            .function
            .set_results(&[Boundary::Register(abi::RETURN_GPRS[0].into())]);
        for (position, &register) in registers.iter().enumerate() {
            let param = self.graph.function.params(self.block)[position];
            frame.set(register, param);
        }
        frame.remember_arguments();
        self.frames.push(frame);
        self.predict_entry_values(&registers);
        self.build_blocks(first)
    }
}
