//! From the addresses in caches and graphs back to what is there.

use bun_engine_vm::code::metadata::GlobalAccess;
use bun_engine_vm::{Gc, GlobalBinding, Shape, ShapeId, SpeculatedType, Value};

use crate::graph::Graph;

/// `id` has just been read from a cache of a code block that is alive.
pub(crate) fn shape_in_cache(id: ShapeId) -> Gc<Shape> {
    assert!(!id.is_none());
    // SAFETY: a collection clears the caches that name a dead shape.
    unsafe { id.decode() }
}

pub(crate) fn binding_in_cache(access: &GlobalAccess) -> Option<&GlobalBinding> {
    let pointer = access.pointer.get();
    // SAFETY: what the pointer is without a shape. The code block keeps the realm alive, which has the binding.
    (pointer != 0 && access.shape.get().is_none())
        .then(|| unsafe { &*core::ptr::with_exposed_provenance(pointer) })
}

/// While the graph is built. `None` for the empty value and for what is no value.
pub(crate) fn constant_of_graph(graph: &Graph, bits: u64) -> Option<Value> {
    let is_value = SpeculatedType::of_immediate(bits).is_some() || graph.cells.contains_key(&bits);
    // SAFETY: the builder roots the cells that it describes.
    is_value.then(|| unsafe { Value::from_bits_or_empty(bits) })?
}
