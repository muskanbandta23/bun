//! From the tree to the graph.

use smallvec::SmallVec;

use super::character::{Encoding, set_of};
use super::graph::{Graph, Id, Kind, Span, Step, Traits};
use super::groups::Groups;
use super::memory::{StackLimit, push};
use super::{Mode, Options};
use crate::Error;
use crate::analysis::Next;
use crate::program::{Direction, Pairs, Width};
use crate::set::{CharRange, Set, SetBuilder};
use crate::surrogate::{ASTRAL, SURROGATES, lead_of, trail_of};
use crate::tree::{self, Group, Node, NodeId, Tree};

pub(super) struct Lowered {
    pub(super) graph: Graph,
    pub(super) root: Id,
    pub(super) pair_count: u16,
}

pub(super) fn lower(tree: &Tree, options: &Options<'_>) -> Result<Lowered, Error> {
    let mut lowering = Lowering {
        tree,
        graph: Graph::new(options.width, tree.node_count() * 2)?,
        groups: Groups::new(tree, options.mode)?,
        encodings: SmallVec::new(),
        terms: Vec::new(),
        direction: Direction::Forward,
        nodes_to_unroll: NODES_TO_UNROLL,
        stack: StackLimit(options.stack_limit),
    };
    lowering
        .encodings
        .try_reserve(tree.set_count())
        .map_err(|_| Error::OutOfMemory)?;
    lowering.encodings.resize(tree.set_count(), None);
    let body = lowering.node(tree.root())?;
    let mut ending = Vec::new();
    if options.mode == Mode::Captures {
        for pairs in lowering.groups.never_set() {
            push(&mut ending, lowering.graph.leaf(Kind::Clear(pairs))?)?;
        }
    }
    push(&mut ending, lowering.graph.leaf(Kind::Accept)?)?;
    Ok(Lowered {
        root: lowering.append_ending(body, &ending, 0)?,
        pair_count: lowering.groups.pair_count(),
        graph: lowering.graph,
    })
}

const NODES_TO_UNROLL: usize = 1 << 10;

pub(super) struct Lowering<'a> {
    pub(super) tree: &'a Tree,
    pub(super) graph: Graph,
    pub(super) groups: Groups,
    encodings: SmallVec<[Option<Encoding>; 8]>,
    /// The children of the nodes that are being built.
    pub(super) terms: Vec<Id>,
    pub(super) direction: Direction,
    pub(super) nodes_to_unroll: usize,
    pub(super) stack: StackLimit,
}

impl Lowering<'_> {
    pub(super) fn node(&mut self, id: NodeId) -> Result<Id, Error> {
        self.stack.check()?;
        match self.tree[id] {
            Node::Sequence(children) => self.sequence(children),
            Node::Alternation(children) => self.alternation(children),
            Node::Capture { group, body } => self.capture(group, body),
            Node::Repeat {
                body,
                min,
                max,
                greedy,
                ..
            } => self.repeat(body, min, max, greedy),
            Node::Look {
                body,
                behind,
                negated,
                ..
            } => self.look(body, behind, negated),
            leaf => self.leaf(leaf),
        }
    }

    pub(super) fn push_term(&mut self, start: usize, term: Id) -> Result<(), Error> {
        self.graph.push_term(&mut self.terms, start, term)
    }

    pub(super) fn finish_sequence(&mut self, start: usize) -> Result<Id, Error> {
        let sequence = self.graph.sequence(&self.terms[start..]);
        self.terms.truncate(start);
        sequence
    }

    pub(super) fn join(&mut self, terms: &[Id]) -> Result<Id, Error> {
        let start = self.terms.len();
        for &term in terms {
            self.push_term(start, term)?;
        }
        self.finish_sequence(start)
    }

    fn sequence(&mut self, children: tree::Span<NodeId>) -> Result<Id, Error> {
        let (start, count) = (self.terms.len(), children.len());
        for index in 0..count {
            let child = self.tree[children][match self.direction {
                Direction::Forward => index,
                Direction::Backward => count - 1 - index,
            }];
            let term = self.node(child)?;
            self.push_term(start, term)?;
        }
        self.finish_sequence(start)
    }

    fn capture(&mut self, group: Group, body: NodeId) -> Result<Id, Error> {
        let Some(pair) = self.groups.pair(group) else {
            return self.node(body);
        };
        let body = self.node(body)?;
        let open = self.graph.leaf(Kind::Open(pair))?;
        let close = self.graph.leaf(Kind::Close(pair))?;
        if self.groups.is_referenced(group) {
            self.graph[close].traits |= Traits::REFERENCED;
        }
        self.groups.close(pair)?;
        self.join(&[open, body, close])
    }

    fn look(&mut self, body: NodeId, behind: bool, negated: bool) -> Result<Id, Error> {
        let direction = match behind {
            true => Direction::Backward,
            false => Direction::Forward,
        };
        let outer = core::mem::replace(&mut self.direction, direction);
        let closed = self.groups.closed_so_far();
        let body = self.node(body)?;
        self.direction = outer;
        if negated {
            self.groups.forget(closed);
        }
        self.finish_look(body, direction, negated)
    }

    #[inline(never)]
    fn finish_look(&mut self, body: Id, direction: Direction, negated: bool) -> Result<Id, Error> {
        let node = self.graph[body];
        match node.kind {
            Kind::Fail if negated => return Ok(Graph::EMPTY),
            Kind::Fail => return Ok(Graph::FAIL),
            _ if negated && node.is(Traits::CANNOT_FAIL) => return Ok(Graph::FAIL),
            _ if node.is(Traits::CANNOT_FAIL) && node.pairs.is_empty() => return Ok(Graph::EMPTY),
            Kind::Steps(steps) if steps.len() == 1 => {
                let step = self.graph.steps[steps.range().start];
                return self.peek(direction, step, negated);
            }
            _ => {}
        }
        let constrains = !negated && direction == self.direction && !node.is(Traits::OPEN);
        self.graph.look(body, direction, negated, constrains)
    }

    /// The unit on that side passes the step, or if `negated` there is none that does.
    fn peek(&mut self, side: Direction, step: Step, negated: bool) -> Result<Id, Error> {
        let step = match negated {
            false => Some(step),
            true => {
                let class = self.graph.class_of_step(step)?;
                let others = self.graph[class].complement()?;
                self.graph.step(&others)?
            }
        };
        let Some(step) = step else {
            return self.edge(side);
        };
        let kind = Kind::Peek {
            side,
            step,
            or_edge: negated,
        };
        if side != self.direction {
            return self.graph.leaf(kind);
        }
        let edge = if negated { Next::EDGE } else { Next::NOTHING };
        let first = self.graph.units_of_step(step) | edge;
        self.graph.closed_leaf(kind, first)
    }

    fn edge(&mut self, side: Direction) -> Result<Id, Error> {
        match side == self.direction {
            true => self.graph.closed_leaf(Kind::Edge(side), Next::EDGE),
            false => self.graph.leaf(Kind::Edge(side)),
        }
    }

    fn line_edge(&mut self, side: Direction) -> Result<Id, Error> {
        let inside = Step::Class(self.graph.add_class(Set::NOT_LINE_TERMINATOR)?);
        self.peek(side, inside, true)
    }

    #[inline(never)]
    fn leaf(&mut self, node: Node) -> Result<Id, Error> {
        match node {
            Node::Text { chars, ignore_case } => self.text(chars, ignore_case),
            Node::Set(set) => {
                let encoding = self.encoding(set)?;
                self.graph.character(encoding, self.direction)
            }
            Node::BackReference {
                groups,
                ignore_case,
            } => self.back_reference(groups, ignore_case),
            Node::InputStart => self.edge(Direction::Backward),
            Node::InputEnd => self.edge(Direction::Forward),
            Node::LineStart => self.line_edge(Direction::Backward),
            Node::LineEnd => self.line_edge(Direction::Forward),
            Node::WordBoundary {
                negated,
                unicode_ignore_case,
            } => {
                let word = match unicode_ignore_case {
                    true => Set::WORD_IGNORING_CASE,
                    false => Set::WORD,
                };
                let other = self.graph.add_class(word.complement()?)?;
                let word = self.graph.add_class(word)?;
                self.graph.leaf(Kind::Boundary {
                    word,
                    other,
                    negated,
                })
            }
            _ => Ok(Graph::EMPTY),
        }
    }

    fn encoding(&mut self, set: tree::SetId) -> Result<Encoding, Error> {
        if let Some(known) = self.encodings[set.index()] {
            return Ok(known);
        }
        let unicode_aware = self.tree.is_unicode_aware();
        let encoding = self.graph.encode(&self.tree[set], unicode_aware)?;
        self.encodings[set.index()] = Some(encoding);
        Ok(encoding)
    }

    fn text(&mut self, chars: tree::Span<u32>, ignore_case: bool) -> Result<Id, Error> {
        let (start, chars) = (self.terms.len(), &self.tree[chars]);
        let mut pending = self.graph.steps.len();
        for index in 0..chars.len() {
            let c = chars[match self.direction {
                Direction::Forward => index,
                Direction::Backward => chars.len() - 1 - index,
            }];
            match self.encode_char(c, ignore_case)? {
                Encoding::Unit(step) => push(&mut self.graph.steps, step)?,
                encoding => {
                    self.push_steps_since(start, pending)?;
                    let character = self.graph.character(encoding, self.direction)?;
                    self.push_term(start, character)?;
                    pending = self.graph.steps.len();
                }
            }
        }
        self.push_steps_since(start, pending)?;
        self.finish_sequence(start)
    }

    fn push_steps_since(&mut self, start: usize, first_step: usize) -> Result<(), Error> {
        let steps = Span::new(first_step..self.graph.steps.len());
        if steps.len() == 0 {
            return Ok(());
        }
        let steps = self.graph.leaf(Kind::Steps(steps))?;
        self.push_term(start, steps)
    }

    #[inline]
    fn encode_char(&mut self, c: u32, ignore_case: bool) -> Result<Encoding, Error> {
        let exact = |value| Step::Unit { value, mask: 0 };
        let has_no_case = c < 0x80 && !(c as u8).is_ascii_alphabetic();
        if ignore_case && !has_no_case {
            return self.encode_equivalents(c);
        }
        let unicode_aware = self.tree.is_unicode_aware();
        Ok(match (self.graph.width, u16::try_from(c)) {
            (Width::Latin1, Ok(unit @ ..=0xff)) => Encoding::Unit(exact(unit)),
            (Width::Latin1, _) => Encoding::Never,
            (Width::Utf16, Err(_)) => Encoding::TwoUnits(exact(lead_of(c)), exact(trail_of(c))),
            (Width::Utf16, Ok(_))
                if unicode_aware && (SURROGATES.first..=SURROGATES.last).contains(&c) =>
            {
                let alone = set_of([CharRange::single(c)].into_iter())?;
                self.graph.encode(&alone, true)?
            }
            (Width::Utf16, Ok(unit)) => Encoding::Unit(exact(unit)),
        })
    }

    fn encode_equivalents(&mut self, c: u32) -> Result<Encoding, Error> {
        let max = match self.graph.width {
            Width::Latin1 => 0xff,
            Width::Utf16 => ASTRAL.last,
        };
        let equivalents = self.tree.case_equivalence().equivalents(c);
        let members = core::iter::once(c).chain(equivalents);
        let members: SmallVec<[u32; 4]> = members.filter(|&it| it <= max).collect();
        match members[..] {
            [] => Ok(Encoding::Never),
            [only] => self.encode_char(only, false),
            [a, b] if a | b <= 0xffff && (a ^ b).is_power_of_two() => {
                Ok(Encoding::Unit(Step::Unit {
                    value: (a | b) as u16,
                    mask: (a ^ b) as u16,
                }))
            }
            _ => {
                let set = set_of(members.into_iter().map(CharRange::single))?;
                self.graph.encode(&set, self.tree.is_unicode_aware())
            }
        }
    }

    fn back_reference(
        &mut self,
        groups: tree::Span<Group>,
        ignore_case: bool,
    ) -> Result<Id, Error> {
        let start = self.graph.referenced_pairs.len();
        for &group in &self.tree[groups] {
            if let Some(pair) = self.groups.visible_pair(group) {
                push(&mut self.graph.referenced_pairs, pair)?;
            }
        }
        let pairs = Span::new(start..self.graph.referenced_pairs.len());
        match pairs.len() {
            0 => Ok(Graph::EMPTY),
            _ => self.graph.leaf(Kind::BackReference { pairs, ignore_case }),
        }
    }

    /// Looks through the groups whose captures nothing reads.
    pub(super) fn without_unread_groups(&self, mut id: NodeId) -> NodeId {
        while let Node::Capture { group, body } = self.tree[id]
            && self.groups.pair(group).is_none()
        {
            id = body;
        }
        id
    }

    fn is_one_character(&self, id: NodeId) -> bool {
        match self.tree[self.without_unread_groups(id)] {
            Node::Set(_) => true,
            Node::Text { chars, .. } => chars.len() == 1,
            _ => false,
        }
    }

    #[inline(never)]
    fn any_of(&mut self, characters: &[NodeId]) -> Result<Id, Error> {
        let mut builder = SetBuilder::default();
        for &id in characters {
            match self.tree[self.without_unread_groups(id)] {
                Node::Set(set) => builder.add_set(&self.tree[set])?,
                Node::Text { chars, ignore_case } => {
                    let c = self.tree[chars][0];
                    builder.add(c)?;
                    if ignore_case {
                        for equal in self.tree.case_equivalence().equivalents(c) {
                            builder.add(equal)?;
                        }
                    }
                }
                _ => unreachable!("the node is one character"),
            }
        }
        let set = builder.finish(None)?;
        let encoding = self.graph.encode(&set, self.tree.is_unicode_aware())?;
        self.graph.character(encoding, self.direction)
    }

    fn alternation(&mut self, children: tree::Span<NodeId>) -> Result<Id, Error> {
        let (start, tree) = (self.terms.len(), self.tree);
        let mut rest = &tree[children];
        let mut earlier = Pairs::NONE;
        while let [child, ..] = *rest {
            let characters = rest.iter().take_while(|&&it| self.is_one_character(it));
            let (taken, after) = rest.split_at(characters.count().max(1));
            rest = after;
            let closed = self.groups.closed_so_far();
            let alternative = if taken.len() > 1 {
                self.any_of(taken)?
            } else {
                self.groups.hide(earlier)?;
                let alternative = self.node(child)?;
                self.groups.reveal();
                alternative
            };
            earlier = earlier.hull(self.graph[alternative].pairs);
            match alternative {
                Graph::FAIL => self.groups.forget(closed),
                _ => self.push_alternative(alternative)?,
            }
        }
        self.finish_alternation(start)
    }

    pub(super) fn push_alternative(&mut self, alternative: Id) -> Result<(), Error> {
        match self.graph[alternative].kind {
            Kind::Alternation(nested) => nested
                .range()
                .try_for_each(|index| push(&mut self.terms, self.graph.children[index])),
            _ => push(&mut self.terms, alternative),
        }
    }

    pub(super) fn finish_alternation(&mut self, start: usize) -> Result<Id, Error> {
        let alternation = self.graph.alternation(&self.terms[start..]);
        self.terms.truncate(start);
        alternation
    }
}
