//! Picks instructions by what values are expected to be, checks what is not known, and drops the checks of what is.

mod arithmetic;
mod checks;
mod comparisons;
mod conversions;

use bun_engine_codegen::analysis::{Cfg, Loop};
use bun_engine_codegen::ir::{
    self, AccessFlags, Block, Effects, Exit, Heap, Load, Op, Origin, State, Store, Table, Type,
    Value as Ssa,
};
use bun_engine_codegen::passes::Analyses;
use smallvec::SmallVec;

use super::usage::Usage;
use crate::graph::{ExitKind, Graph, Kind, Knowledge, Known, SavedKnowledge, implied_type};

struct Specializer<'a> {
    graph: &'a mut Graph,
    usage: &'a Usage,
    knowledge: Knowledge,
    /// The instruction that is being replaced.
    at: Ssa,
    origin: Origin,
    state: State,
    changed_control_flow: bool,
}

const MAX_HEAPS_OF_LOOP: usize = 12;

#[derive(Clone, Default)]
struct LoopWrites {
    heaps: SmallVec<[Heap; MAX_HEAPS_OF_LOOP]>,
}

impl LoopWrites {
    fn add(&mut self, heap: Heap) {
        if self.heaps.len() == MAX_HEAPS_OF_LOOP {
            self.heaps[0] = Heap::EVERYTHING;
        } else if !self.heaps.contains(&heap) {
            self.heaps.push(heap);
        }
    }
}

fn writes_of_loops(graph: &Graph, cfg: &Cfg) -> Table<Loop, LoopWrites> {
    let mut writes = Table::filled(cfg.loops().len(), LoopWrites::default());
    for &block in cfg.order().iter().rev() {
        let Some(own) = cfg.loop_of(block).expand() else {
            continue;
        };
        let written = graph
            .function
            .instructions(block)
            .map(|value| graph.function.effects(value).writes);
        written
            .filter(|&heap| heap != Heap::NOTHING)
            .for_each(|heap| writes[own].add(heap));
        let data = &cfg.loops()[own];
        if let Some(outer) = data.parent.expand().filter(|_| data.header == block) {
            writes[own]
                .heaps
                .clone()
                .into_iter()
                .for_each(|heap| writes[outer].add(heap));
        }
    }
    writes
}

struct Arrival {
    knowledge: SavedKnowledge,
    params: SmallVec<[Known; 4]>,
}

pub(super) fn run(graph: &mut Graph, analyses: &mut Analyses, usage: &Usage) {
    let cfg = analyses.cfg(&mut graph.function);
    let loop_writes = writes_of_loops(graph, cfg);
    let mut arrivals: Table<Block, Option<Arrival>> = Table::default();
    (0..graph.function.block_count()).for_each(|_| _ = arrivals.push(None));
    let mut specializer = Specializer {
        graph,
        usage,
        knowledge: Knowledge::default(),
        at: Ssa::NONE,
        origin: Origin::default(),
        state: State::NONE,
        changed_control_flow: false,
    };
    for &block in cfg.order() {
        match arrivals[block].take() {
            Some(arrival) => specializer.enter(cfg, block, &arrival, &loop_writes),
            None if cfg.predecessors(block).is_empty() => specializer.knowledge.clear(),
            None => continue,
        }
        specializer.walk(block);
        specializer.leave(cfg, block, &mut arrivals);
    }
    if specializer.changed_control_flow {
        analyses.invalidate();
        analyses.cfg(&mut graph.function);
    }
    graph.function.resolve_aliases();
}

impl Specializer<'_> {
    fn enter(
        &mut self,
        cfg: &Cfg,
        block: Block,
        arrival: &Arrival,
        loop_writes: &Table<Loop, LoopWrites>,
    ) {
        self.knowledge.restore(self.graph, &arrival.knowledge);
        if !cfg
            .predecessors(block)
            .iter()
            .any(|&edge| cfg.is_backward(&self.graph.function, edge))
        {
            for &known in &arrival.params {
                *self.knowledge.entry(self.graph, known.root) = known;
            }
            return;
        }
        match cfg
            .loop_headed_by(block)
            .filter(|&headed| cfg.loops()[headed].is_natural)
        {
            Some(headed) => {
                loop_writes[headed]
                    .heaps
                    .iter()
                    .for_each(|&heap| self.knowledge.note_write(self.graph, heap));
                self.knowledge.note_effects(self.graph, Effects::NONE, true);
            }
            None => self.knowledge.clear(),
        }
    }

    fn leave(&mut self, cfg: &Cfg, block: Block, arrivals: &mut Table<Block, Option<Arrival>>) {
        let condition = match *self.graph.function.op(self.graph.function.last(block)) {
            Op::Branch(condition) => Some(condition),
            _ => None,
        };
        for (position, edge) in self.graph.function.successors(block).enumerate() {
            self.graph.function.resolve_args(edge);
            if cfg.is_backward(&self.graph.function, edge) {
                continue;
            }
            let implied =
                condition.and_then(|condition| implied_type(self.graph, condition, position == 0));
            let before = implied.map(|(subject, ty)| {
                let known = self.knowledge.entry(self.graph, subject);
                let before = *known;
                (known.ty, known.alias) = (known.ty & ty, Ssa::NONE);
                before
            });
            let target = self.graph.function.target(edge);
            let function = &self.graph.function;
            let pairs = function.args(edge).iter().zip(function.params(target));
            let tagged = pairs.filter(|&(_, &param)| function.type_of(param) == Type::I64);
            let params: SmallVec<[Known; 4]> = tagged
                .map(|(&arg, &param)| self.knowledge.passed_on(self.graph, arg, param))
                .collect();
            match &mut arrivals[target] {
                Some(arrival) => {
                    self.knowledge
                        .merge_into(self.graph, &mut arrival.knowledge);
                    arrival
                        .params
                        .iter_mut()
                        .zip(&params)
                        .for_each(|(known, other)| known.widen(self.graph, other));
                }
                first => {
                    let mut knowledge = SavedKnowledge::default();
                    self.knowledge.save(&mut knowledge, |_| true);
                    *first = Some(Arrival { knowledge, params });
                }
            }
            if let Some(before) = before {
                *self.knowledge.entry(self.graph, before.root) = before;
            }
        }
    }

    fn walk(&mut self, block: Block) {
        let mut value = self.graph.function.first(block);
        while !value.is_none() {
            self.graph.function.resolve_operands(value);
            (self.at, self.origin, self.state) = (
                value,
                self.graph.function.origin(value),
                self.graph.function.state_of(value),
            );
            match self.graph.node(value) {
                Some((kind, data)) => self.visit_node(kind, data),
                None => self.visit_instruction(),
            }
            value = self.graph.function.next(value);
        }
    }

    fn visit_node(&mut self, kind: Kind, data: u32) {
        let operand = |position| {
            self.graph
                .function
                .operands(self.at)
                .get(position)
                .copied()
                .unwrap_or(Ssa::NONE)
        };
        let (first, second) = (operand(0), operand(1));
        let replacement = match kind {
            Kind::AsInt32 => self.as_int32(first, self.conversion_failure(data)),
            Kind::AsInt52 => self.as_int52(first, self.conversion_failure(data)),
            Kind::AsDouble => self.as_double(first, self.conversion_failure(data)),
            Kind::AsBoolean => self.as_boolean(first, self.conversion_failure(data)),
            Kind::Truncate => self.truncate(first),
            Kind::UInt32ToNumber => self.uint32_to_number(first),
            Kind::NumberAdd | Kind::NumberSubtract | Kind::NumberMultiply => {
                self.add_or_multiply(kind, first, second)
            }
            Kind::NumberDivide => self.divide(first, second),
            Kind::NumberRemainder => self.remainder(first, second),
            Kind::NumberNegate => self.negate(first),
            Kind::NumberToNumber => self.check_type(
                first,
                bun_engine_vm::SpeculatedType::BOXED_NUMBER,
                ExitKind::ExoticObject,
            ),
            Kind::Compare => self.compare(crate::graph::Relation::from_data(data), first, second),
            Kind::LooseEqual => self.loose_equal(first, second),
            Kind::StrictEqual => self.strict_equal(first, second),
            Kind::ToBoolean => self.to_boolean(first),
            Kind::EqualNull => self.equal_null(first),
            _ => return self.refine(kind, data, first, second),
        };
        self.graph.function.replace(self.at, replacement);
    }

    fn visit_instruction(&mut self) {
        match *self.graph.function.op(self.at) {
            Op::Load {
                kind: Load::Full,
                address,
                offset,
                access,
            } if access.has(AccessFlags::ALWAYS_READABLE) => {
                let heap = if access.has(AccessFlags::IMMUTABLE) {
                    Heap::NOTHING
                } else {
                    access.heap()
                };
                let base = self.graph.root(address);
                let ty = self.graph.function.type_of(self.at);
                match self
                    .knowledge
                    .field(base, offset, heap)
                    .filter(|&known| self.graph.function.type_of(known) == ty)
                {
                    Some(known) => self.graph.function.replace(self.at, known),
                    None => self.knowledge.learn_field(base, offset, heap, self.at),
                }
            }
            Op::Store {
                kind,
                args: [stored, address],
                offset,
                access,
            } => {
                self.knowledge.note_write(self.graph, access.heap());
                if kind == Store::Full {
                    self.knowledge.learn_field(
                        self.graph.root(address),
                        offset,
                        access.heap(),
                        stored,
                    );
                }
            }
            Op::Branch(condition) => {
                if let Some(constant) = self.graph.function.int_constant(condition) {
                    let block = self.graph.function.block_of(self.at);
                    self.graph.function.keep_only_successor(
                        self.graph
                            .function
                            .successor(block, usize::from(constant == 0)),
                    );
                    self.changed_control_flow = true;
                }
            }
            Op::Binary(op, [lhs, rhs])
                if self.graph.function.op(lhs).is_constant()
                    && self.graph.function.op(rhs).is_constant() =>
            {
                let folded = self.ir().binary(op, lhs, rhs);
                self.graph.function.replace(self.at, folded);
            }
            Op::Check(condition, _) if self.graph.function.is_int_constant(condition, 0) => {
                self.graph.function.remove(self.at)
            }
            _ => {
                self.knowledge
                    .note_effects(self.graph, self.graph.function.effects(self.at), false)
            }
        }
    }

    // Helpers

    fn ir(&mut self) -> ir::Builder<'_> {
        ir::Builder::before(&mut self.graph.function, self.at)
    }

    fn node(&mut self, kind: Kind, operands: &[Ssa], data: u32) -> Ssa {
        let state = if kind.effects().has(ir::EffectFlags::EXITS) {
            self.state
        } else {
            State::NONE
        };
        let value = self.ir().node(
            kind.id(),
            kind.gives().machine_type(),
            operands,
            data,
            state,
        );
        self.knowledge
            .note_effects(self.graph, kind.effects(), kind.can_collect());
        value
    }

    fn exit(&mut self, kind: ExitKind) -> Exit {
        self.graph.exit(kind, self.origin, self.state)
    }

    fn exit_if(&mut self, condition: Ssa, kind: ExitKind) {
        if !self.graph.function.is_int_constant(condition, 0) {
            let exit = self.exit(kind);
            self.ir().check(condition, exit);
        }
    }

    fn has_exited(&self, kind: ExitKind) -> bool {
        self.graph.has_exited(self.origin, kind)
    }
}
