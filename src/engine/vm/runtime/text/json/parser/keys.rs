//! The sequences of keys that objects have had, as a tree. A node says which key is likely to come next, so that it
//! takes one comparison to read it, and what the shape of an object that ends there is.

use bun_alloc::AllocError;
use bun_engine_text::{Identifier, PropertyName, Text, names, scan};

use crate::heap::{Scope, Survivors};
use crate::object::SnugShape;
use crate::{Gc, GlobalObject, Shape, ShapeId};

/// The offset of a node in bytes. None is kept outside the tree but by a running parser, so it is always valid.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub(super) struct NodeId(u32);

impl NodeId {
    /// Matches no text.
    pub(super) const NONE: NodeId = NodeId::at(0);
    /// Where an object is whose keys are not followed in the tree.
    pub(super) const OUTSIDE: NodeId = NodeId::at(1);
    /// The low bits that are always clear.
    pub(super) const SPARE_BITS: u32 = size_of::<Node>() as u32 - 1;

    const fn at(index: usize) -> NodeId {
        NodeId((index * size_of::<Node>()) as u32)
    }

    #[inline(always)]
    pub(super) fn bits(self) -> u32 {
        self.0
    }

    /// # Safety
    /// What `bits` has returned to this parser.
    #[inline(always)]
    pub(super) unsafe fn from_bits(bits: u32) -> NodeId {
        NodeId(bits)
    }
}

const HALF: usize = 16;

#[repr(C, align(64))]
pub(super) struct Node {
    /// The key in quotes, and the colon, if that fits and reads the same in every encoding and dialect. Else zeros.
    text: [[u8; HALF]; 2],
    /// A bit for each byte of `text` that counts.
    mask: u32,
    length: u8,
    /// The number of keys up to and including this one.
    pub(super) depth: u8,
    /// The key that followed the last time.
    pub(super) next: NodeId,
    /// The first key of the last object in the value.
    pub(super) child: NodeId,
    parent: NodeId,
    /// The id, which does not keep it alive, with `NOT_SNUG` unless it is a `SnugShape`.
    shape: u32,
    name: Identifier,
}

const NOT_SNUG: u32 = 1;

const _: () = assert!(size_of::<Node>() == 64);

impl Node {
    fn new(name: Identifier, parent: NodeId, depth: u8) -> Node {
        let mut node = Node {
            text: Default::default(),
            mask: 1,
            length: 0,
            depth,
            next: NodeId::NONE,
            child: NodeId::NONE,
            parent,
            shape: NOT_SNUG,
            name,
        };
        if depth != 0
            && let Text::Latin1(key) = node.name.name().text()
            && let Some(text) = node.text.as_flattened_mut().get_mut(..key.len() + 3)
            && key.is_ascii()
            && scan::json_string_run_len(key) == key.len()
            && node.name != names::__proto__
        {
            text[0] = b'"';
            text[1..=key.len()].copy_from_slice(key);
            text[key.len() + 1..].copy_from_slice(b"\":");
            node.length = text.len() as u8;
            node.mask = u32::MAX >> (u32::BITS - u32::from(node.length));
        }
        node
    }

    /// The length of the key with all that goes with it, if `bytes` start with that. The first one is a quote.
    #[inline(always)]
    pub(super) fn find_in(&self, bytes: [&[u8; HALF]; 2]) -> Option<u32> {
        let low = scan::equal_mask(bytes[0], &self.text[0]);
        let high = scan::equal_mask(bytes[1], &self.text[1]);
        (!(low | high << HALF) & self.mask == 0).then_some(self.length.into())
    }

    pub(super) fn name(&self) -> PropertyName<'_> {
        self.name.name()
    }

    pub(super) fn shape(&self) -> Option<Gc<Shape>> {
        let id = ShapeId::from_bits(self.shape & !NOT_SNUG);
        // SAFETY: cleared by `reconcile` before it is freed.
        (!id.is_none()).then(|| unsafe { id.decode() })
    }

    /// It is alive, and has `depth` slots.
    #[inline(always)]
    pub(super) fn snug_shape(&self) -> Option<SnugShape> {
        // SAFETY: what the bit says.
        (self.shape & NOT_SNUG == 0)
            .then(|| unsafe { SnugShape::new_unchecked(ShapeId::from_bits(self.shape)) })
    }
}

pub(super) struct KeyTree {
    nodes: Vec<Node>,
    /// The nodes by parent and name, with linear probing.
    slots: Vec<NodeId>,
    /// By the address of the global object.
    roots: Vec<(usize, NodeId)>,
    /// The nodes that got their shape since the last collection.
    shaped: Vec<NodeId>,
}

impl core::ops::Index<NodeId> for KeyTree {
    type Output = Node;

    #[inline(always)]
    fn index(&self, id: NodeId) -> &Node {
        debug_assert!(
            (id.0 as usize) < size_of_val(&*self.nodes) && id.0 & NodeId::SPARE_BITS == 0
        );
        // SAFETY: valid.
        unsafe { &*self.nodes.as_ptr().byte_add(id.0 as usize) }
    }
}

impl core::ops::IndexMut<NodeId> for KeyTree {
    #[inline(always)]
    fn index_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.0 as usize / size_of::<Node>()]
    }
}

impl Default for KeyTree {
    fn default() -> KeyTree {
        let special = || Node::new(PropertyName::EMPTY.to_identifier(), NodeId::NONE, 0);
        KeyTree {
            nodes: vec![special(), special()],
            slots: vec![NodeId::NONE; 64],
            roots: Vec::new(),
            shaped: Vec::new(),
        }
    }
}

impl KeyTree {
    pub(super) const MAX_DEPTH: u8 = 128;
    const MAX_NODES: usize = u32::MAX as usize / size_of::<Node>();
    const MAX_PROBES: usize = 32;
    const MAX_KEPT_BY_COLLECTION: usize = 1 << 12;
    const MAX_KEPT_BETWEEN_TEXTS: usize = 1 << 16;

    /// The parent of the first keys of the objects of a realm.
    #[inline(always)]
    pub(super) fn root(&mut self, global: &GlobalObject) -> NodeId {
        match self.roots.last() {
            Some(&(address, root))
                if address == global.address()
                    && self.nodes.len() <= Self::MAX_KEPT_BETWEEN_TEXTS =>
            {
                root
            }
            _ => self.switch_root(global.address()),
        }
    }

    #[cold]
    fn switch_root(&mut self, global: usize) -> NodeId {
        if self.nodes.len() > Self::MAX_KEPT_BETWEEN_TEXTS {
            *self = KeyTree::default();
        }
        self[NodeId::OUTSIDE].child = NodeId::NONE;
        let root = match self.roots.iter().position(|root| root.0 == global) {
            Some(position) => self.roots.swap_remove(position).1,
            None => {
                let empty = PropertyName::EMPTY.to_identifier();
                self.nodes.push(Node::new(empty, NodeId::NONE, 0));
                NodeId::at(self.nodes.len() - 1)
            }
        };
        self.roots.push((global, root));
        root
    }

    #[inline(always)]
    fn probe(slot_count: usize, parent: NodeId, hash: u32) -> impl Iterator<Item = usize> {
        let mixed =
            (u64::from(parent.0) << 32 | u64::from(hash)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let first = (mixed >> 32) as usize;
        (first..).map(move |slot| slot & (slot_count - 1))
    }

    /// The node for the key `text` after `parent`, unless the tree cannot take it. `hash` is that of `text`.
    #[inline]
    pub(super) fn follow(&mut self, parent: NodeId, text: Text<'_>, hash: u32) -> Option<NodeId> {
        for slot in Self::probe(self.slots.len(), parent, hash).take(Self::MAX_PROBES) {
            let id = self.slots[slot];
            if id == NodeId::NONE {
                return self.add(slot, parent, text, hash).ok();
            }
            if self[id].parent == parent && self[id].name().text() == text {
                return Some(id);
            }
        }
        None
    }

    #[inline(never)]
    fn add(
        &mut self,
        slot: usize,
        parent: NodeId,
        text: Text<'_>,
        hash: u32,
    ) -> Result<NodeId, AllocError> {
        let depth = self[parent].depth + 1;
        if depth > Self::MAX_DEPTH || self.nodes.len() >= Self::MAX_NODES {
            return Err(AllocError);
        }
        self.nodes.try_reserve(1).map_err(|_| AllocError)?;
        let name = bun_engine_text::Atom::with_hash(text, hash);
        let mut node = Node::new(name.into(), parent, depth);
        // The values of a map are much alike.
        node.child = self[parent].child;
        let id = NodeId::at(self.nodes.len());
        self.nodes.push(node);
        self.slots[slot] = id;
        if self.nodes.len() * 2 > self.slots.len() {
            self.grow()?;
        }
        Ok(id)
    }

    #[cold]
    fn grow(&mut self) -> Result<(), AllocError> {
        let count = self.slots.len() * 4;
        let mut slots = Vec::new();
        slots.try_reserve_exact(count).map_err(|_| AllocError)?;
        slots.resize(count, NodeId::NONE);
        for (index, node) in self.nodes.iter().enumerate().filter(|it| it.1.depth != 0) {
            let mut probe = Self::probe(count, node.parent, node.name().hash());
            if let Some(slot) = probe.find(|&slot| slots[slot] == NodeId::NONE) {
                slots[slot] = NodeId::at(index);
            }
        }
        self.slots = slots;
        Ok(())
    }

    /// The keys up to and including that of `node`, the last one first.
    pub(super) fn path(&self, node: NodeId) -> impl Iterator<Item = PropertyName<'_>> {
        core::iter::successors(Some(&self[node]), |node| Some(&self[node.parent]))
            .take_while(|node| node.depth != 0)
            .map(Node::name)
    }

    /// `shape` has the keys of the path for its properties.
    pub(super) fn set_shape(&mut self, node: NodeId, shape: &Shape) {
        assert!(shape.slot_count() == usize::from(self[node].depth));
        if self.shaped.try_reserve(1).is_ok() {
            self.shaped.push(node);
            let tag = if SnugShape::fits(shape) { 0 } else { NOT_SNUG };
            self[node].shape = shape.id().bits() | tag;
        }
    }

    pub(super) fn reconcile(&mut self, survivors: &Survivors) {
        // SAFETY: only the address is looked at.
        let is_live =
            |global| survivors.contains(unsafe { Gc::<GlobalObject>::from_address(global) });
        if survivors.scope() == Scope::Full
            || self.nodes.len() > Self::MAX_KEPT_BY_COLLECTION
            || !self.roots.iter().all(|root| is_live(root.0))
        {
            *self = KeyTree::default();
        }
        while let Some(id) = self.shaped.pop() {
            if !self[id].shape().is_some_and(|it| survivors.contains(it)) {
                self[id].shape = NOT_SNUG;
            }
        }
    }
}
