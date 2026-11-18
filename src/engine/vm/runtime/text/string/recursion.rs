use core::cell::{Cell, RefCell};
use core::ptr::NonNull;

use crate::{Cx, FastHashSet, Gc, JsObject, JsResult, Vm};

/// The addresses of the objects that have a guard, which keeps them alive.
#[derive(Default)]
pub(super) struct Visited {
    first: Cell<usize>,
    others: RefCell<FastHashSet<usize>>,
}

/// Says that an object is being converted to a string, so that it is left out where it contains itself.
pub struct StringRecursionGuard {
    vm: NonNull<Vm>,
    object: Gc<JsObject>,
}

impl StringRecursionGuard {
    /// `None` if it has one already.
    #[inline]
    pub fn enter(cx: &Cx, object: Gc<JsObject>) -> JsResult<Option<StringRecursionGuard>> {
        cx.check_stack()?;
        let (visited, address) = (&cx.services.text.strings.recursion, object.address());
        match visited.first.get() {
            0 => visited.first.set(address),
            first if first == address || !visited.others.borrow_mut().insert(address) => {
                return Ok(None);
            }
            _ => {}
        }
        let vm = NonNull::from(cx.vm());
        Ok(Some(StringRecursionGuard { vm, object }))
    }
}

impl Drop for StringRecursionGuard {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: made and dropped inside a call that has the `Vm`.
        let visited = &unsafe { self.vm.as_ref() }.services.text.strings.recursion;
        if visited.first.get() == self.object.address() {
            visited.first.set(0);
        } else {
            visited.others.borrow_mut().remove(&self.object.address());
        }
    }
}
