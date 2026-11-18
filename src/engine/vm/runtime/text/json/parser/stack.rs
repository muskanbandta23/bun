use super::keys::NodeId;
use crate::{Cx, JsResult, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Object,
    Array,
    Outside,
}

/// The innermost object or array that is open.
#[derive(Clone, Copy)]
pub(super) struct Scope {
    /// Where the members start on the stack.
    base: u32,
    /// The last key, which for an array is one of an object around it, and above it the kind.
    node_and_kind: u32,
}

impl Scope {
    #[inline(always)]
    fn new(base: usize, kind: Kind, node: NodeId) -> Scope {
        Scope {
            base: base as u32,
            node_and_kind: node.0 | (kind as u32) << NodeId::BITS,
        }
    }

    #[inline(always)]
    pub(super) fn outside(root: NodeId) -> Scope {
        Scope::new(0, Kind::Outside, root)
    }

    #[inline(always)]
    pub(super) fn kind(self) -> Kind {
        if self.node_and_kind < 1 << NodeId::BITS {
            Kind::Object
        } else if self.node_and_kind < 2 << NodeId::BITS {
            Kind::Array
        } else {
            Kind::Outside
        }
    }

    #[inline(always)]
    pub(super) fn node(self) -> NodeId {
        NodeId(self.node_and_kind & ((1 << NodeId::BITS) - 1))
    }

    /// Of an object.
    #[inline(always)]
    pub(super) fn set_node(&mut self, node: NodeId) {
        self.node_and_kind = node.0;
    }
}

/// The members of the open objects and arrays, each run of them after the `Scope` to go back to.
#[derive(Default)]
#[cfg_attr(rootcheck, rootcheck::allow("empty unless collection is deferred"))]
pub(super) struct Stack {
    slots: Vec<Value>,
    length: usize,
}

#[cold]
#[inline(never)]
fn grow(cx: &Cx, slots: &mut Vec<Value>) -> JsResult<()> {
    let additional = slots.len().max(64);
    if slots.len() + additional > u32::MAX as usize || slots.try_reserve_exact(additional).is_err() {
        return Err(cx.throw_out_of_memory());
    }
    slots.resize(slots.len() + additional, Value::NULL);
    Ok(())
}

impl Stack {
    #[inline(always)]
    pub(super) fn clear(&mut self) {
        self.length = 0;
    }

    #[inline(always)]
    pub(super) fn push(&mut self, cx: &Cx, value: Value) -> JsResult<()> {
        if self.length >= self.slots.len() {
            let mut slots = core::mem::take(&mut self.slots);
            let result = grow(cx, &mut slots);
            self.slots = slots;
            result?;
        }
        self.slots[self.length] = value;
        self.length += 1;
        Ok(())
    }

    #[inline(always)]
    pub(super) fn enter(
        &mut self,
        cx: &Cx,
        outer: Scope,
        kind: Kind,
        node: NodeId,
    ) -> JsResult<Scope> {
        let bits = u64::from(outer.node_and_kind & (u32::MAX >> 2)) << 32 | u64::from(outer.base);
        self.push(cx, Value::double(f64::from_bits(bits)))?;
        Ok(Scope::new(self.length, kind, node))
    }

    #[inline(always)]
    pub(super) fn members(&mut self, scope: Scope) -> &mut [Value] {
        &mut self.slots[scope.base as usize..self.length]
    }

    /// Returns the outer one.
    #[inline(always)]
    pub(super) fn leave(&mut self, scope: Scope) -> Scope {
        self.length = (scope.base as usize).wrapping_sub(1);
        let bits = self.slots[self.length].double_unchecked().to_bits();
        Scope {
            base: bits as u32,
            node_and_kind: (bits >> 32) as u32,
        }
    }
}
