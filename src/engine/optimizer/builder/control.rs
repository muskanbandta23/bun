//! Blocks, edges and the parameters where ways meet.

use std::rc::Rc;

use bun_engine_bytecode::{BlockIndex, EdgeKind, Instruction, Register};
use bun_engine_codegen::ir::{Block, Edge, State, Successor, Type, Value as Ssa};
use smallvec::SmallVec;

use super::frame::Junction;
use super::{Builder, Built, Refusal};
use crate::graph::{ExitKind, Kind, Known};

const MAX_VALUES: usize = 400_000;

impl Builder<'_> {
    /// Translates the innermost frame, which control enters at `first`.
    pub(super) fn build_blocks(&mut self, first: BlockIndex) -> Built {
        let liveness = Rc::clone(&self.frame().liveness);
        let order = self.order_from(first);
        let junctions = &mut self.frame_mut().junctions;
        junctions
            .iter_mut()
            .for_each(|junction| junction.is_done = true);
        order
            .iter()
            .for_each(|block| junctions[block.index()].is_done = false);
        let mut order = order.into_iter().peekable();
        if !liveness.graph().predecessors(first).is_empty() {
            self.goto(first);
        }
        while let Some(block) = order.next() {
            if !self.block.is_none() || self.enter_junction(block) {
                self.translate_block(block)?;
            }
            self.frame_mut().junctions[block.index()].is_done = true;
            if !self.block.is_none() {
                let next = BlockIndex(block.0 + 1);
                if order.peek() != Some(&next) || liveness.graph().predecessors(next).len() != 1 {
                    self.goto(next);
                }
            }
        }
        Ok(())
    }

    /// Reverse postorder, which follows the code where it can.
    fn order_from(&self, first: BlockIndex) -> Vec<BlockIndex> {
        let graph = self.frame().liveness.graph();
        let mut is_seen = vec![false; graph.len()];
        let (mut order, mut stack) = (Vec::with_capacity(graph.len()), vec![(first, 0)]);
        is_seen[first.index()] = true;
        while let Some((block, visited)) = stack.last_mut() {
            let mut successors: SmallVec<[BlockIndex; 4]> = graph
                .successors(*block)
                .iter()
                .filter(|edge| edge.kind != EdgeKind::Throw)
                .map(|edge| edge.block)
                .collect();
            successors.sort_unstable_by_key(|successor| core::cmp::Reverse(successor.0));
            match successors.get(*visited) {
                Some(&successor) => {
                    *visited += 1;
                    if !core::mem::replace(&mut is_seen[successor.index()], true) {
                        stack.push((successor, 0));
                    }
                }
                None => order.push(stack.pop().unwrap().0),
            }
        }
        order.reverse();
        order
    }

    fn translate_block(&mut self, block: BlockIndex) -> Built {
        let frame = self.frames.last_mut().unwrap();
        let (code_block, liveness) = (frame.code_block, Rc::clone(&frame.liveness));
        frame.current = block;
        frame.live_before.clear();
        liveness.walk_backwards(code_block.body(), block, &mut self.walk, |_, _, live| {
            frame.live_before.extend_from_slice(live.words());
        });
        let range = liveness.graph().range(block);
        let mut offset = range.start;
        while offset < range.end && !self.block.is_none() {
            let (instruction, size) = Instruction::read(code_block.body().instructions(), offset);
            self.begin_instruction(offset);
            self.translate(instruction, offset + size)?;
            let frame = self.frame_mut();
            frame
                .live_before
                .truncate(frame.live_before.len() - frame.words);
            offset += size;
        }
        if self.graph.function.value_count() > MAX_VALUES {
            Err(Refusal::TooLarge)
        } else {
            Ok(())
        }
    }

    fn block_of(&mut self, target: BlockIndex) -> Block {
        let junction = &mut self.frames.last_mut().unwrap().junctions[target.index()];
        *junction
            .block
            .get_or_insert_with(|| self.graph.function.new_block())
    }

    pub(super) fn target_at(&self, offset: u32) -> BlockIndex {
        self.frame().liveness.graph().block_at(offset)
    }

    pub(super) fn goto(&mut self, target: BlockIndex) {
        let block = self.block_of(target);
        self.ir().jump(block, &[]);
        self.arrive(self.graph.function.successor(self.block, 0), target);
        self.block = Block::NONE;
    }

    /// Goes to `target` if the I32 `condition` is `expected`, and otherwise on to the instruction at `next`.
    pub(super) fn branch(&mut self, condition: Ssa, expected: bool, target: u32, next: u32) {
        let (taken, other) = (self.target_at(target), self.target_at(next));
        let (then, or) = if expected {
            (taken, other)
        } else {
            (other, taken)
        };
        if let Some(constant) = self.graph.function.int_constant(condition) {
            return self.goto(if constant != 0 { then } else { or });
        }
        let blocks = [then, or].map(|target| self.block_of(target));
        self.ir().branch(condition, blocks[0], blocks[1]);
        for (position, target) in [then, or].into_iter().enumerate() {
            let undo = self.assume_condition(condition, position == 0);
            self.arrive(self.graph.function.successor(self.block, position), target);
            self.undo_assumption(undo);
        }
        self.block = Block::NONE;
    }

    pub(super) fn switch(&mut self, value: Ssa, cases: &[(i64, u32)], default: u32) {
        self.switch_with(value, cases, default, |_, _| {});
    }

    /// `prepare` sets the registers for the way at a position.
    pub(super) fn switch_with(
        &mut self,
        value: Ssa,
        cases: &[(i64, u32)],
        default: u32,
        prepare: impl Fn(&mut Self, usize),
    ) {
        let targets: SmallVec<[BlockIndex; 16]> = cases
            .iter()
            .map(|&(_, offset)| offset)
            .chain([default])
            .map(|offset| self.target_at(offset))
            .collect();
        let blocks: SmallVec<[Block; 16]> = targets
            .iter()
            .map(|&target| self.block_of(target))
            .collect();
        let successors: SmallVec<[(i64, Successor<'_>); 16]> = cases
            .iter()
            .zip(&blocks)
            .map(|(&(key, _), &block)| (key, block.into()))
            .collect();
        self.ir()
            .switch(value, &successors, blocks[cases.len()].into());
        for (position, &target) in targets.iter().enumerate() {
            prepare(self, position);
            self.arrive(self.graph.function.successor(self.block, position), target);
        }
        self.block = Block::NONE;
    }

    pub(super) fn force_exit(&mut self, kind: ExitKind) {
        let state = self.state();
        self.insert_node(Kind::ForceExit, &[], kind as u32, state);
        self.end_block();
    }

    /// After a node that ends the block and has no successors.
    pub(super) fn end_block(&mut self) {
        self.graph.function.clear_successors(self.block);
        self.block = Block::NONE;
    }

    fn arrive(&mut self, edge: Edge, target: BlockIndex) {
        let frame = self.frames.last_mut().unwrap();
        let mut junction = core::mem::take(&mut frame.junctions[target.index()]);
        self.scratch.clear();
        self.scratch.extend(
            frame
                .carried(frame.liveness.live_in(target))
                .map(|register| self.graph.root(frame.get(register))),
        );
        self.join(&mut junction, edge);
        self.frame_mut().junctions[target.index()] = junction;
    }

    /// The values that the edge brings are in `scratch`.
    pub(super) fn join(&mut self, junction: &mut Junction, edge: Edge) {
        let graph = &mut self.graph;
        junction.follows_effect |= self.needs_invalidation_point;
        for &position in &junction.params {
            graph
                .function
                .push_arg(edge, self.scratch[position as usize]);
        }
        if junction.is_entered {
            debug_assert!(junction.values.iter().zip(&self.scratch).enumerate().all(
                |(position, (old, new))| {
                    old == new || junction.params.contains(&(position as u32))
                }
            ));
            return;
        }
        if junction.edges.is_empty() {
            junction.values.clone_from(&self.scratch);
            self.knowledge.save(&mut junction.knowledge, |_| true);
            return junction.edges.push(edge);
        }
        let block = junction.block.unwrap();
        let mut about_params: SmallVec<[_; 8]> = SmallVec::new();
        for (position, (old, &new)) in junction.values.iter_mut().zip(&self.scratch).enumerate() {
            if *old == new {
                continue;
            }
            let earlier = junction.knowledge.find(*old).copied();
            let is_param = graph.function.block_of(*old) == block;
            let param = if is_param {
                *old
            } else {
                graph.function.add_param(block, Type::I64)
            };
            let mut known = self.knowledge.passed_on(graph, new, param);
            known.widen(
                graph,
                &earlier.unwrap_or_else(|| Known::inherent(graph, *old)),
            );
            known.alias = param;
            about_params.push(known);
            let seen = graph.prediction(*old) | graph.prediction(new);
            graph.predict(param, seen);
            if !is_param {
                junction
                    .edges
                    .iter()
                    .for_each(|&earlier| graph.function.push_arg(earlier, *old));
                graph.function.push_arg(edge, new);
                junction.params.push(position as u32);
                *old = param;
            }
        }
        self.knowledge.merge_into(graph, &mut junction.knowledge);
        about_params
            .into_iter()
            .for_each(|known| junction.knowledge.add(known));
        junction.edges.push(edge);
    }

    /// Returns false if nothing leads there.
    fn enter_junction(&mut self, target: BlockIndex) -> bool {
        let frame = self.frames.last_mut().unwrap();
        let mut junction = core::mem::take(&mut frame.junctions[target.index()]);
        let Some(block) = junction.block else {
            return false;
        };
        let liveness = Rc::clone(&frame.liveness);
        let is_translated =
            |edge: &bun_engine_bytecode::Edge| frame.junctions[edge.block.index()].is_done;
        let is_reached_again = !liveness
            .graph()
            .predecessors(target)
            .iter()
            .all(is_translated);
        self.knowledge.restore(&self.graph, &junction.knowledge);
        if is_reached_again {
            self.add_loop_params(&mut junction, target);
            self.knowledge
                .enter_loop(&self.graph, self.graph.function.params(block));
        }
        let frame = self.frames.last_mut().unwrap();
        for (register, &value) in frame
            .carried(liveness.live_in(target))
            .zip(&junction.values)
        {
            frame.set(register, value);
        }
        self.needs_invalidation_point = junction.follows_effect || is_reached_again;
        junction.is_entered = true;
        junction.knowledge = Default::default();
        frame.junctions[target.index()] = junction;
        self.block = block;
        self.checkpoint = State::NONE;
        true
    }

    fn add_loop_params(&mut self, junction: &mut Junction, header: BlockIndex) {
        let frame = self.frames.last().unwrap();
        let block = junction.block.unwrap();
        let assigned = frame.assigned_on_the_way_back_to(header);
        for (position, register) in frame.carried(frame.liveness.live_in(header)).enumerate() {
            let old = junction.values[position];
            if assigned.contains(register) && self.graph.function.block_of(old) != block {
                let param = self.graph.function.add_param(block, Type::I64);
                junction
                    .edges
                    .iter()
                    .for_each(|&edge| self.graph.function.push_arg(edge, old));
                let prediction = self.graph.prediction(old);
                self.graph.predict(param, prediction);
                junction.params.push(position as u32);
                junction.values[position] = param;
            }
        }
    }
}

pub(super) struct Assigned {
    lowest: i32,
    is_assigned: Vec<bool>,
}

impl Assigned {
    fn contains(&self, register: Register) -> bool {
        self.is_assigned[(register.offset() - self.lowest) as usize]
    }
}

impl super::frame::Frame {
    /// What the blocks write from which `header` is reached without going through code that has been translated.
    fn assigned_on_the_way_back_to(&self, header: BlockIndex) -> Assigned {
        let (graph, body) = (self.liveness.graph(), self.code_block.body());
        let lowest = -(body.locals() as i32);
        let highest = self.argument_registers().last().map_or(0, Register::offset);
        let mut assigned = Assigned {
            lowest,
            is_assigned: vec![false; (highest - lowest + 1) as usize],
        };
        let mut is_seen = vec![false; graph.len()];
        let mut pending = vec![header];
        is_seen[header.index()] = true;
        while let Some(block) = pending.pop() {
            for (_, instruction) in graph.instructions(body, block) {
                instruction.for_each_def(body, |run, _| {
                    run.registers().for_each(|register| {
                        assigned.is_assigned[(register.offset() - lowest) as usize] = true
                    });
                });
            }
            // Another way into the cycle may bring other values for anything.
            if block != header && !self.junctions[block.index()].edges.is_empty() {
                assigned.is_assigned.fill(true);
                break;
            }
            for edge in graph.predecessors(block) {
                if !self.junctions[edge.block.index()].is_done
                    && !core::mem::replace(&mut is_seen[edge.block.index()], true)
                {
                    pending.push(edge.block);
                }
            }
        }
        assigned
    }
}
