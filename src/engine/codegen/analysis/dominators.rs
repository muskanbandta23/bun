use super::Cfg;
use crate::ir::{Block, Function, Table, Value};

/// A block in the tree of dominators. Ranks are positions in the order from one, and zero is a root above the entries.
#[derive(Clone, Copy)]
pub(crate) struct Ancestry {
    pub(crate) parent: u32,
    /// An ancestor from which any other is a logarithmic number of steps away: Myers, "An Applicative Random-Access
    /// Stack". It is the parent, or what two jumps from the parent reach.
    pub(crate) jump: u32,
    depth: u32,
}

/// A forest: entries, and blocks that several entries reach independently, are roots.
#[derive(Default)]
pub struct Dominators {
    parent: Table<Block, Block>,
    jump: Table<Block, Block>,
    first_child: Table<Block, Block>,
    next_sibling: Table<Block, Block>,
    first_root: Option<Block>,
    /// When a walk of the forest enters and leaves the block.
    interval: Table<Block, (u32, u32)>,
    pre_order: Vec<Block>,
    by_rank: Vec<Ancestry>,
}

fn child_of(tree: &[Ancestry], parent: u32) -> Ancestry {
    let above = tree[parent as usize];
    let jumped = tree[above.jump as usize];
    let are_even = above.depth - jumped.depth == jumped.depth - tree[jumped.jump as usize].depth;
    Ancestry {
        parent,
        jump: if are_even { jumped.jump } else { parent },
        depth: above.depth + 1,
    }
}

#[inline]
fn common_ancestor(tree: &[Ancestry], mut a: u32, mut b: u32) -> u32 {
    const SHORT_WALK: u32 = 8;
    for _ in 0..SHORT_WALK {
        if a == b {
            return a;
        }
        if a > b {
            a = tree[a as usize].parent;
        } else {
            b = tree[b as usize].parent;
        }
    }
    distant_common_ancestor(tree, a, b)
}

fn distant_common_ancestor(tree: &[Ancestry], mut a: u32, mut b: u32) -> u32 {
    if tree[a as usize].depth < tree[b as usize].depth {
        core::mem::swap(&mut a, &mut b);
    }
    let depth = tree[b as usize].depth;
    while tree[a as usize].depth > depth {
        let Ancestry { parent, jump, .. } = tree[a as usize];
        a = if tree[jump as usize].depth >= depth {
            jump
        } else {
            parent
        };
    }
    while a != b {
        let (left, right) = (tree[a as usize], tree[b as usize]);
        (a, b) = if left.jump != right.jump {
            (left.jump, right.jump)
        } else {
            (left.parent, right.parent)
        };
    }
    a
}

impl Dominators {
    pub fn new(function: &Function, cfg: &Cfg) -> Self {
        let mut dominators = Self::default();
        dominators.compute(function, cfg);
        dominators
    }

    /// Cooper, Harvey and Kennedy, "A Simple, Fast Dominance Algorithm".
    pub fn compute(&mut self, function: &Function, cfg: &Cfg) {
        const ROOT: Ancestry = Ancestry {
            parent: 0,
            jump: 0,
            depth: 0,
        };
        let tree = &mut self.by_rank;
        tree.clear();
        tree.resize(cfg.order().len() + 1, ROOT);
        let backward_edges_matter = cfg.loops().values().any(|data| !data.is_natural);
        let mut is_first_round = true;
        loop {
            let mut changed = is_first_round;
            for (rank, &block) in (1..).zip(cfg.order()) {
                let mut sources = cfg
                    .predecessors(block)
                    .iter()
                    .map(|&edge| cfg.position(function.source(edge)) + 1);
                let mut common = sources.next().filter(|&first| first < rank).unwrap_or(0);
                for mut other in sources {
                    if other >= rank && is_first_round {
                        break;
                    }
                    // What comes later still has the ancestors of the round before.
                    while other > rank {
                        let Ancestry { parent, jump, .. } = tree[other as usize];
                        other = if jump >= rank { jump } else { parent };
                    }
                    if other != rank {
                        common = common_ancestor(tree, common, other);
                    }
                }
                changed |= tree[rank as usize].parent != common;
                tree[rank as usize] = child_of(tree, common);
            }
            if !changed || !backward_edges_matter {
                break;
            }
            is_first_round = false;
        }

        let count = function.block_count();
        self.parent.refill(count, Block::NONE);
        self.jump.refill(count, Block::NONE);
        self.first_child.refill(count, Block::NONE);
        self.next_sibling.refill(count, Block::NONE);
        let block_at = |rank: u32| {
            rank.checked_sub(1)
                .map_or(Block::NONE, |position| cfg.order()[position as usize])
        };
        let mut first_root = Block::NONE;
        for (&block, ancestry) in cfg.order().iter().zip(&tree[1..]).rev() {
            (self.parent[block], self.jump[block]) =
                (block_at(ancestry.parent), block_at(ancestry.jump));
            let first = match self.parent[block].expand() {
                Some(parent) => &mut self.first_child[parent],
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

    /// The position in `pre_order` after the last block that `block` dominates.
    #[inline]
    pub fn end_of_subtree(&self, block: Block) -> u32 {
        self.interval[block].1
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
            a = match self.jump[a].expand() {
                Some(jump) if !self.dominates(jump, b) => jump,
                _ => self.parent(a)?,
            };
        }
        Some(a)
    }

    pub(crate) fn by_rank(&self) -> &[Ancestry] {
        &self.by_rank
    }
}
