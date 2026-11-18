//! Finds the choices that have to be remembered.

use super::Next;
use crate::Error;
use crate::compile::character::{has_all_pairs_and_halves, set_of};
use crate::compile::graph::{Form, Graph, Id, Kind, MAX_REACH, Span, Traits};
use crate::compile::memory::StackLimit;
use crate::program::{ClassId, Stride, UNBOUNDED, Width};
use crate::set::Set;
use crate::surrogate::{ASTRAL, SURROGATES};

/// When it can help to go back into a node that has succeeded.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Retry {
    Never,
    /// What follows fails only if the turn of a loop has passed nothing.
    IfEmpty,
    Possibly,
}

#[derive(Clone, Copy)]
struct After<'a> {
    /// What can come next if the rest of the pattern is to succeed.
    next: &'a Next,
    retry: Retry,
}

const END: After<'static> = After {
    next: &Next::ANYTHING,
    retry: Retry::Never,
};

pub(crate) fn decide_backtracking(
    graph: &mut Graph,
    root: Id,
    stack: StackLimit,
) -> Result<(), Error> {
    Analysis { graph, stack }.node(root, END)
}

struct Analysis<'a> {
    graph: &'a mut Graph,
    stack: StackLimit,
}

const LEFT_BEHIND: Traits = Traits::REENTERED.union(Traits::FRAMES);

impl Analysis<'_> {
    fn node(&mut self, id: Id, mut after: After<'_>) -> Result<(), Error> {
        self.stack.check()?;
        let node = &self.graph[id];
        let ends_alike = node.min == node.max && !node.is(Traits::REFERENCED);
        if ends_alike || after.retry == Retry::IfEmpty && node.min != 0 {
            after.retry = Retry::Never;
        }
        let mut left = match node.kind {
            Kind::Sequence(children) => self.sequence(children, after)?,
            Kind::Alternation(children) => self.alternation(id, children, after)?,
            Kind::Repeat { .. } => self.repeat(id, after)?,
            Kind::Turn(body) => {
                self.node(body, after)?;
                self.graph[body].traits & LEFT_BEHIND
            }
            Kind::Look { body, .. } => return self.node(body, END),
            Kind::Run { .. } => return self.run(id, after),
            _ => return Ok(()),
        };
        if after.retry == Retry::Never {
            left.remove(Traits::REENTERED);
        }
        let node = &mut self.graph[id];
        if left == Traits::FRAMES && !node.is(Traits::ACCEPTS) {
            left = Traits::CUTS;
        }
        node.traits |= left;
        Ok(())
    }

    fn sequence(&mut self, children: Span, after: After<'_>) -> Result<Traits, Error> {
        let (mut next, mut retry) = (*after.next, after.retry);
        let mut left = Traits::empty();
        for index in children.range().rev() {
            let child = self.graph.children[index];
            self.node(child, After { next: &next, retry })?;
            let node = &self.graph[child];
            left |= node.traits & LEFT_BEHIND;
            retry = match (node.kind, retry) {
                (Kind::NotEmpty, Retry::Never) => Retry::IfEmpty,
                (_, Retry::IfEmpty) if node.min != 0 => Retry::Possibly,
                _ if node.is(Traits::CANNOT_FAIL) => retry,
                _ => Retry::Possibly,
            };
            next = self.graph.first_before(child, &next);
        }
        Ok(left)
    }

    fn alternation(&mut self, id: Id, children: Span, after: After<'_>) -> Result<Traits, Error> {
        let children = self.rearrange(id, children, after);
        let (mut later, mut left) = (Next::NOTHING, Traits::empty());
        for index in children.range().rev() {
            let child = self.graph.children[index];
            self.node(child, after)?;
            let first = self.graph.first(child);
            let first_to_succeed = self.graph.first_before(child, after.next);
            let node = &mut self.graph[child];
            let is_last = index + 1 == children.range().end;
            let is_final = match after.retry {
                Retry::Never => true,
                Retry::IfEmpty => node.min != 0,
                Retry::Possibly => false,
            };
            if !node.is(Traits::OPEN) && !first.intersects(&later) {
                node.traits |= Traits::EXCLUSIVE;
            } else if !is_last && !is_final {
                node.traits |= Traits::CONTINUED;
                left |= Traits::REENTERED;
            }
            left |= node.traits & LEFT_BEHIND;
            later = later | first_to_succeed;
        }
        Ok(left)
    }

    /// Drops the alternatives that are never tried, and puts one last that would only be tried in vain before others.
    #[inline(never)]
    fn rearrange(&mut self, id: Id, children: Span, after: After<'_>) -> Span {
        let graph = &mut *self.graph;
        let range = children.range();
        if after.retry == Retry::Never {
            let always = range
                .clone()
                .find(|&index| graph[graph.children[index]].is(Traits::CANNOT_FAIL));
            let kept = Span::new(range.start..always.map_or(range.end, |index| index + 1));
            graph[id].kind = Kind::Alternation(kept);
            return kept;
        }
        let open = range
            .clone()
            .rfind(|&index| graph[graph.children[index]].is(Traits::OPEN));
        if let Some(open) = open.filter(|&index| index + 1 != range.end) {
            let first = graph.first_before(graph.children[open], after.next);
            let mut later = graph.children[open + 1..range.end].iter();
            if !later.any(|&it| graph.first_before(it, after.next).intersects(&first)) {
                graph.children[open..range.end].rotate_left(1);
            }
        }
        children
    }

    fn repeat(&mut self, id: Id, after: After<'_>) -> Result<Traits, Error> {
        let Kind::Repeat {
            body,
            min,
            mut max,
            mut greedy,
            ..
        } = self.graph[id].kind
        else {
            unreachable!("the node is a loop")
        };
        let of_body = self.graph[body];
        let first = self.graph.first(body);
        let is_distinct = !of_body.is(Traits::OPEN) && !first.intersects(after.next);
        if !greedy && after.retry == Retry::Never {
            max = min;
        }
        greedy |= min == max;
        let is_final = match after.retry {
            Retry::Never => true,
            Retry::IfEmpty => min == 0,
            Retry::Possibly => false,
        };
        let next = self.graph.first_before(body, after.next) | *after.next;
        let retry = match greedy && is_final && min <= 1 {
            true => Retry::Never,
            false => Retry::Possibly,
        };
        self.node(body, After { next: &next, retry })?;

        let (form, left) = if self.graph[body].is(Traits::REENTERED) {
            (Form::Stacked, LEFT_BEHIND)
        } else if min == max || is_distinct || greedy && is_final {
            greedy = true;
            (Form::Possessive, Traits::empty())
        } else if !greedy {
            (Form::Extended, Traits::REENTERED)
        } else if self.graph.turns_are_alike(body) {
            (Form::Stepped, Traits::REENTERED)
        } else {
            (Form::Stacked, LEFT_BEHIND)
        };
        self.graph[id].kind = Kind::Repeat {
            body,
            min,
            max,
            greedy,
            form,
        };
        Ok(left)
    }

    #[inline(never)]
    fn run(&mut self, id: Id, after: After<'_>) -> Result<(), Error> {
        let Kind::Run {
            class,
            max,
            mut code_points,
            mut greedy,
            ..
        } = self.graph[id].kind
        else {
            unreachable!("the node is a run")
        };
        if !greedy && after.retry == Retry::Never {
            self.graph[id].kind = Kind::Empty;
            return Ok(());
        }
        let is_distinct = !self.graph.first(id).intersects(after.next);
        greedy |= is_distinct;
        let is_possessive = is_distinct || greedy && after.retry != Retry::Possibly;
        let can_stop_in_pair = !is_possessive && after.next.intersects_wide(SURROGATES);
        if max == UNBOUNDED && !can_stop_in_pair && has_all_pairs_and_halves(&self.graph[class]) {
            code_points = false;
        }
        let filter = if is_possessive {
            None
        } else if greedy && !code_points {
            self.filter_for_giving_back(class, after.next)?
        } else if !greedy && max == UNBOUNDED {
            self.passed_at_once(class, after.next, code_points)?
        } else {
            None
        };
        let node = &mut self.graph[id];
        node.traits.set(Traits::REENTERED, !is_possessive);
        node.kind = Kind::Run {
            class,
            max,
            code_points,
            greedy,
            filter,
        };
        Ok(())
    }

    fn filter_for_giving_back(
        &mut self,
        class: ClassId,
        next: &Next,
    ) -> Result<Option<ClassId>, Error> {
        if self.graph[class].latin1().is_subset_of(next.latin1()) {
            return Ok(None);
        }
        let filter = match !next.has_wide() || self.graph.width == Width::Latin1 {
            true => Set::from_latin1(*next.latin1()),
            false => set_of(next.ranges())?,
        };
        self.graph.add_class(filter).map(Some)
    }

    /// The members of the class that the rest of the pattern cannot start with.
    fn passed_at_once(
        &mut self,
        class: ClassId,
        next: &Next,
        code_points: bool,
    ) -> Result<Option<ClassId>, Error> {
        let members = &self.graph[class];
        let latin1 = *members.latin1() - *next.latin1();
        let passed = if !next.has_wide() || self.graph.width == Width::Latin1 {
            members.with_latin1(latin1)?
        } else {
            let pairs = (code_points && next.intersects_wide(SURROGATES)).then_some(ASTRAL);
            members.difference(&set_of(next.ranges().chain(pairs))?)?
        };
        match passed.is_empty() {
            true => Ok(None),
            false => self.graph.add_class(passed).map(Some),
        }
    }
}

impl Graph {
    /// Whether all turns have the same length and their captures at the same distances.
    pub(crate) fn turns_are_alike(&self, body: Id) -> bool {
        let node = &self[body];
        let has_fixed_captures = node.pairs.is_empty()
            || self.terms(&body).iter().all(|&term| {
                let term = &self[term];
                term.is_bookkeeping() || term.pairs.is_empty() && term.min == term.max
            }) && !node.is(Traits::REFERENCED)
                && node.min <= MAX_REACH;
        node.min == node.max && Stride::of_units(node.min).is_some() && has_fixed_captures
    }
}
