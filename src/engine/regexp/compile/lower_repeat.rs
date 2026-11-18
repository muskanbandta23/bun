//! Quantifiers, from the tree to the graph.

use smallvec::SmallVec;

use super::graph::{Graph, Id, Kind, Span, Step, Traits};
use super::lower::Lowering;
use super::memory::{push, reserve};
use crate::Error;
use crate::program::{ClassId, UNBOUNDED};
use crate::tree::NodeId;

const STEPS_UNROLLED: u32 = 16;
const CODE_POINTS_UNROLLED: u32 = 4;
const NODES_UNROLLED_AT_ONCE: usize = 32;

fn minus(max: u32, turns: u32) -> u32 {
    match max {
        UNBOUNDED => UNBOUNDED,
        _ => max - turns,
    }
}

impl Lowering<'_> {
    pub(super) fn repeat(
        &mut self,
        source: NodeId,
        min: u32,
        max: u32,
        greedy: bool,
    ) -> Result<Id, Error> {
        let (closed, nodes) = (self.groups.closed_so_far(), self.graph.node_count());
        let body = self.node(source)?;
        let node = self.graph[body];
        if node.max == 0 {
            if min == 0 {
                self.groups.forget(closed);
                return Ok(Graph::EMPTY);
            }
            return Ok(body);
        }
        let copies = min.saturating_sub(1) as usize;
        let cost = (self.graph.node_count() - nodes).saturating_mul(copies);
        let is_plain = matches!(node.kind, Kind::Steps(_) | Kind::CodePoint(_));
        let is_affordable = cost <= NODES_UNROLLED_AT_ONCE.min(self.nodes_to_unroll);
        // A copy that fails leaves the captures of the one before it overwritten.
        if is_plain || copies == 0 || !is_affordable || node.is(Traits::REFERENCED) {
            return self.repeat_lowered(body, min, max, greedy);
        }
        self.nodes_to_unroll -= cost;
        let start = self.terms.len();
        let mut last = body;
        for _ in 0..copies {
            self.push_term(start, last)?;
            self.groups.forget(closed);
            last = self.node(source)?;
        }
        let rest = self.repeat_lowered(last, 1, minus(max, min - 1), greedy)?;
        self.push_term(start, rest)?;
        self.finish_sequence(start)
    }

    #[inline(never)]
    fn repeat_lowered(&mut self, body: Id, min: u32, max: u32, greedy: bool) -> Result<Id, Error> {
        let node = self.graph[body];
        match node.kind {
            _ if (min, max) == (1, 1) => Ok(body),
            Kind::Steps(steps) if steps.len() == 1 => self.repeat_step(steps, min, max, greedy),
            Kind::Steps(steps) if min != 0 && node.min.saturating_mul(min) <= STEPS_UNROLLED => {
                self.repeat_steps(steps, min, max, greedy)
            }
            Kind::CodePoint(class) => self.repeat_code_point(body, class, min, max, greedy),
            _ => self.repeat_group(body, min, max, greedy),
        }
    }

    fn copy_steps(&mut self, steps: Span, copies: u32) -> Result<Id, Error> {
        reserve(&mut self.graph.steps, steps.len() * copies as usize)?;
        let start = self.graph.steps.len();
        for _ in 0..copies {
            self.graph.steps.extend_from_within(steps.range());
        }
        let copied = Span::new(start..self.graph.steps.len());
        self.graph.leaf(Kind::Steps(copied))
    }

    fn repeat_step(&mut self, steps: Span, min: u32, max: u32, greedy: bool) -> Result<Id, Error> {
        let step = self.graph.steps[steps.range().start];
        let required = match min {
            0 => Graph::EMPTY,
            1..=STEPS_UNROLLED => self.copy_steps(steps, min)?,
            count => {
                let class = self.graph.class_of_step(step)?;
                self.graph.leaf(Kind::StepCount { class, count })?
            }
        };
        if min == max {
            return Ok(required);
        }
        let optional = self.run_of_units(step, minus(max, min), greedy)?;
        self.join(&[required, optional])
    }

    fn run_of_units(&mut self, step: Step, max: u32, greedy: bool) -> Result<Id, Error> {
        let class = self.graph.class_of_step(step)?;
        self.graph.leaf(Kind::Run {
            class,
            max,
            code_points: false,
            greedy,
            filter: None,
        })
    }

    fn repeat_steps(&mut self, steps: Span, min: u32, max: u32, greedy: bool) -> Result<Id, Error> {
        let required = self.copy_steps(steps, min)?;
        if min == max {
            return Ok(required);
        }
        let body = self.graph.leaf(Kind::Steps(steps))?;
        let optional = self.repeat_group(body, 0, minus(max, min), greedy)?;
        self.join(&[required, optional])
    }

    fn repeat_code_point(
        &mut self,
        body: Id,
        class: ClassId,
        min: u32,
        max: u32,
        greedy: bool,
    ) -> Result<Id, Error> {
        let start = self.terms.len();
        if min <= CODE_POINTS_UNROLLED {
            for _ in 0..min {
                let copy = self.graph.duplicate(body)?;
                self.push_term(start, copy)?;
            }
        } else {
            let copy = self.graph.duplicate(body)?;
            let required = self.graph.repeat(copy, min, min, true)?;
            self.push_term(start, required)?;
        }
        let max = minus(max, min);
        if max != 0 {
            let optional = if greedy || max == UNBOUNDED {
                let kind = Kind::Run {
                    class,
                    max,
                    code_points: true,
                    greedy,
                    filter: None,
                };
                self.graph.leaf_with_first(kind, self.graph.first(body))?
            } else {
                self.repeat_group(body, 0, max, greedy)?
            };
            self.push_term(start, optional)?;
        }
        self.finish_sequence(start)
    }

    fn repeat_group(
        &mut self,
        body: Id,
        mut min: u32,
        max: u32,
        greedy: bool,
    ) -> Result<Id, Error> {
        let node = self.graph[body];
        let always_matches = node.min == 0 && node.is(Traits::CANNOT_FAIL);
        if always_matches && greedy && max == UNBOUNDED && node.pairs.is_empty() {
            min = 0;
        }
        let body = if node.min == 0 && min != max {
            let check = self.graph.leaf(Kind::NotEmpty)?;
            let checked = self.append_check(body, check)?;
            self.graph.turn(checked, min != 0)?
        } else {
            body
        };
        if (min, max) != (0, 1) {
            return self.graph.repeat(body, min, max, greedy);
        }
        let start = self.terms.len();
        if !greedy {
            push(&mut self.terms, Graph::EMPTY)?;
        }
        self.push_alternative(body)?;
        if greedy {
            push(&mut self.terms, Graph::EMPTY)?;
        }
        self.finish_alternation(start)
    }

    /// Puts `check` at the end of each way through `id` that can be empty.
    fn append_check(&mut self, id: Id, check: Id) -> Result<Id, Error> {
        self.stack.check()?;
        let node = self.graph[id];
        match node.kind {
            _ if node.min != 0 => return Ok(id),
            Kind::Alternation(children) => {
                for index in children.range() {
                    let checked = self.append_check(self.graph.children[index], check)?;
                    self.graph.children[index] = match checked == check {
                        true => self.graph.duplicate(check)?,
                        false => checked,
                    };
                }
            }
            Kind::Sequence(children) => {
                let last = children
                    .range()
                    .rfind(|&index| !self.graph[self.graph.children[index]].is_bookkeeping());
                match last.map(|index| self.graph.children[index]) {
                    Some(last) if matches!(self.graph[last].kind, Kind::Alternation(_)) => {
                        self.append_check(last, check)?;
                    }
                    _ => return self.join(&[id, check]),
                }
            }
            _ => return self.join(&[id, check]),
        }
        self.graph.refresh(id);
        Ok(id)
    }

    /// Puts `ending` after `id`, and into its last alternatives so that they need not meet again.
    pub(super) fn append_ending(&mut self, id: Id, ending: &[Id], depth: u32) -> Result<Id, Error> {
        const MAX_TERMS: usize = 4;
        const MAX_DEPTH: u32 = 8;
        let (start, terms) = (self.terms.len(), self.graph.terms(&id));
        let kept = terms
            .iter()
            .rposition(|&it| !self.graph[it].is_bookkeeping());
        let (before, closing) = terms.split_at(kept.map_or(0, |last| last + 1));
        if let Some(&last) = before.last()
            && let Kind::Alternation(children) = self.graph[last].kind
            && closing.len() + ending.len() <= MAX_TERMS
            && depth < MAX_DEPTH
        {
            let longer: SmallVec<[Id; MAX_TERMS]> = closing.iter().chain(ending).copied().collect();
            reserve(&mut self.terms, before.len())?;
            self.terms.extend_from_slice(before);
            for index in children.range() {
                let alternative = self.graph.children[index];
                let own_pairs = self.graph[alternative].pairs;
                let mut ended = self.append_ending(alternative, &longer, depth + 1)?;
                if longer.contains(&ended) {
                    ended = self.graph.duplicate(ended)?;
                }
                self.graph[ended].pairs = own_pairs;
                self.graph.children[index] = ended;
            }
            self.graph.refresh(last);
        } else {
            self.push_term(start, id)?;
            for &term in ending {
                self.push_term(start, term)?;
            }
        }
        self.finish_sequence(start)
    }
}
