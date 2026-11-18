//! Which captures are kept, and which can have been set at the place that is being lowered.

use super::Mode;
use super::memory::{filled, push};
use crate::Error;
use crate::program::Pairs;
use crate::tree::{Group, Node, Tree};

#[derive(Clone, Copy, Default)]
struct OfGroup {
    pair: Option<u16>,
    is_referenced: bool,
}

pub(super) struct Groups {
    /// From group one.
    groups: Vec<OfGroup>,
    is_closed: Vec<bool>,
    closed: Vec<u16>,
    /// Those of the alternatives before the ones that enclose the place, ascending.
    hidden: Vec<Pairs>,
}

impl Groups {
    pub(super) fn new(tree: &Tree, mode: Mode) -> Result<Groups, Error> {
        let mut groups = filled(tree.group_count().into(), OfGroup::default())?;
        if !groups.is_empty() {
            for (_, node) in tree.nodes() {
                if let Node::BackReference { groups: read, .. } = node {
                    for &group in &tree[read] {
                        groups[usize::from(group) - 1].is_referenced = true;
                    }
                }
            }
        }
        let mut pair_count = 0;
        for group in &mut groups {
            if mode == Mode::Captures || group.is_referenced {
                group.pair = Some(pair_count);
                pair_count += 1;
            }
        }
        Ok(Groups {
            groups,
            is_closed: filled(pair_count.into(), false)?,
            closed: Vec::new(),
            hidden: Vec::new(),
        })
    }

    pub(super) fn pair_count(&self) -> u16 {
        self.is_closed.len() as u16
    }

    pub(super) fn pair(&self, group: Group) -> Option<u16> {
        self.groups[usize::from(group) - 1].pair
    }

    pub(super) fn is_referenced(&self, group: Group) -> bool {
        self.groups[usize::from(group) - 1].is_referenced
    }

    pub(super) fn close(&mut self, pair: u16) -> Result<(), Error> {
        self.is_closed[usize::from(pair)] = true;
        push(&mut self.closed, pair)
    }

    pub(super) fn closed_so_far(&self) -> usize {
        self.closed.len()
    }

    /// Those closed since then are not set after all.
    pub(super) fn forget(&mut self, closed_so_far: usize) {
        for pair in self.closed.drain(closed_so_far..) {
            self.is_closed[usize::from(pair)] = false;
        }
    }

    pub(super) fn hide(&mut self, pairs: Pairs) -> Result<(), Error> {
        let after_last = self.hidden.last().map_or(0, |last| last.end);
        let kept_in_order = Pairs {
            first: after_last,
            end: after_last,
        };
        push(
            &mut self.hidden,
            if pairs.is_empty() {
                kept_in_order
            } else {
                pairs
            },
        )
    }

    pub(super) fn reveal(&mut self) {
        self.hidden.pop();
    }

    pub(super) fn visible_pair(&self, group: Group) -> Option<u16> {
        let pair = self.pair(group)?;
        let after = self.hidden.partition_point(|hidden| hidden.end <= pair);
        let is_hidden = self.hidden.get(after).is_some_and(|it| it.first <= pair);
        (self.is_closed[usize::from(pair)] && !is_hidden).then_some(pair)
    }

    pub(super) fn never_set(&self) -> impl Iterator<Item = Pairs> {
        let mut first = 0;
        let runs = self.is_closed.chunk_by(|a, b| a == b);
        runs.filter_map(move |run| {
            let pairs = Pairs {
                first,
                end: first + run.len() as u16,
            };
            first = pairs.end;
            (!run[0]).then_some(pairs)
        })
    }
}
