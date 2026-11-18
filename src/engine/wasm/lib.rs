//! The JavaScript API of WebAssembly, and its objects in the heap.

use bun_engine_vm::{Cx, RealmSlot};

/// False if the slot belongs to another crate.
pub fn initialize_slot(cx: &Cx, slot: RealmSlot) -> bool {
    if slot == RealmSlot::webassembly_object {
        cx.global().init_webassembly_object(cx.new_object());
    }
    slot == RealmSlot::webassembly_object
}
