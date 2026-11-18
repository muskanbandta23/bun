use core::cell::{Cell, RefCell};

use crate::{Cx, FastHashSet, Gc, JsObject, JsResult, JsString};

/// The addresses of the objects whose elements are being joined, which the stack keeps alive.
#[derive(Default)]
pub(super) struct InProgress {
    outermost: Cell<usize>,
    others: RefCell<FastHashSet<usize>>,
}

impl JsObject {
    /// An object that contains itself is the empty string the second time.
    #[inline(always)]
    pub fn unless_being_joined(
        &self,
        cx: &mut Cx,
        join: impl FnOnce(&mut Cx) -> JsResult<Gc<JsString>>,
    ) -> JsResult<Gc<JsString>> {
        cx.check_stack()?;
        let (address, state) = (self.address(), &cx.services.iteration.being_joined);
        let outermost = state.outermost.get();
        if outermost == 0 {
            state.outermost.set(address);
        } else if outermost == address || !state.others.borrow_mut().insert(address) {
            return Ok(cx.small_strings.empty());
        }
        let result = join(cx);
        let state = &cx.services.iteration.being_joined;
        if outermost == 0 {
            state.outermost.set(0);
        } else {
            state.others.borrow_mut().remove(&address);
        }
        result
    }
}
