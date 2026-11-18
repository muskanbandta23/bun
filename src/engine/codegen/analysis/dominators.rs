use super::Cfg;
use crate::ir::{Block, Function, Table, Value};

/// A forest: entries, and blocks that several entries reach independently, are roots.
#[derive(Default)]
pub struct Dominators {
    parent: Table<Block, Block>,
    first_child: Table<Block, Block>,
    next_sibling: Table<Block, Block>,
    first_root: Option<Block>,
    /// When a walk of the forest enters and leaves the block.
    interval: Table<Block, (u32, u32)>,
    pre_order: Vec<Block>,
    by_position: Vec<u32>,
}

impl Dominators {
    pub fn new(function: &Function, cfg: &Cfg) -> Self {
        let mut dominators = Self::default();
        dominators.compute(function, cfg);
        dominators
    }

    /// Cooper, Harvey and Kennedy, "A Simple, Fast Dominance Algorithm". Positions count from one, and zero is a root
    /// above the entries.
    pub fn compute(&mut self, function: &Function, cfg: &Cfg) {
        const UNKNOWN: u32 = u32::MAX;
        let parents = &mut self.by_position;
        parents.clear();
        parents.resize(cfg.order().len() + 1, UNKNOWN);
        parents[0] = 0;
        let backward_edges_matter = cfg.loops().values().any(|data| !data.is_natural);
        loop {
            let mut changed = false;
            for (index, &block) in cfg.order().iter().enumerate() {
                let mut common = if cfg.predecessors(block).is_empty() {
                    0
                } else {
                    UNKNOWN
                };
                for &edge in cfg.predecessors(block) {
                    let mut other = cfg.position(function.source(edge)) + 1;
                    if parents[other as usize] == UNKNOWN {
                        continue;
                    }
                    if common == UNKNOWN {
                        common = other;
                    }
                    while common != other {
                        if common > other {
                            common = parents[common as usize];
                        } else {
                            other = parents[other as usize];
                        }
                    }
                }
                changed |= core::mem::replace(&mut parents[index + 1], common) != common;
            }
            if !changed || !backward_edges_matter {
                break;
            }
        }

        let count = function.block_count();
        self.parent.refill(count, Block::NONE);
        self.first_child.refill(count, Block::NONE);
        self.next_sibling.refill(count, Block::NONE);
        let mut first_root = Block::NONE;
        for (index, &block) in cfg.order().iter().enumerate().rev() {
            let first = match parents[index + 1].checked_sub(1) {
                Some(position) => {
                    self.parent[block] = cfg.order()[position as usize];
                    &mut self.first_child[self.parent[block]]
                }
                None => &mut first_root,
            };
            self.next_sibling[block] = core::mem::replace(first, block);
        }
        self.first_root = first_root.expand();
        self.number(count);
    }

    fn number(&mut self, count: usize) {
        self.interval.refill(count, (u32::MAX, 0));
        self.pre_order.clear();
        let mut next = self.first_root;
        while let Some(mut block) = next {
            self.interval[block].0 = self.pre_order.len() as u32;
            self.pre_order.push(block);
            next = self.first_child[block].expand();
            while next.is_none() {
                self.interval[block].1 = self.pre_order.len() as u32;
                next = self.next_sibling[block].expand();
                match self.parent[block].expand() {
                    Some(parent) if next.is_none() => block = parent,
                    _ => break,
                }
            }
        }
    }

    /// The nearest block that dominates `block`, other than itself.
    #[inline]
    pub fn parent(&self, block: Block) -> Option<Block> {
        self.parent[block].expand()
    }

    pub fn children(&self, block: Block) -> impl Iterator<Item = Block> {
        core::iter::successors(self.first_child[block].expand(), |&child| {
            self.next_sibling[child].expand()
        })
    }

    /// The blocks that one block dominates follow it directly.
    pub fn pre_order(&self) -> &[Block] {
        &self.pre_order
    }

    /// A block dominates itself.
    #[inline]
    pub fn dominates(&self, a: Block, b: Block) -> bool {
        let ((start, end), (position, _)) = (self.interval[a], self.interval[b]);
        (start..end).contains(&position)
    }

    #[inline]
    pub fn strictly_dominates(&self, a: Block, b: Block) -> bool {
        a != b && self.dominates(a, b)
    }

    /// Whether the value can be used at the end of the block.
    #[inline]
    pub fn is_available_after(&self, function: &Function, value: Value, block: Block) -> bool {
        function
            .block_of(value)
            .expand()
            .is_none_or(|home| self.dominates(home, block))
    }

    pub fn common_dominator(&self, mut a: Block, b: Block) -> Option<Block> {
        while !self.dominates(a, b) {
            a = self.parent(a)?;
        }
        Some(a)
    }
}
