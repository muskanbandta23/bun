use core::cmp::Ordering;

use crate::{Cx, JsResult, JsString};

impl JsString {
    /// `None` unless both are atoms.
    #[inline(always)]
    pub fn equals_if_atoms(&self, other: &JsString) -> Option<bool> {
        (self.is_atom() && other.is_atom()).then(|| self.fiber() == other.fiber())
    }

    #[inline(always)]
    pub fn equals(&self, cx: &Cx, other: &JsString) -> JsResult<bool> {
        if let (Some(a), Some(b)) = (self.try_impl(), other.try_impl()) {
            return Ok(a == b);
        }
        Ok(self.len() == other.len() && self.rope_equals(cx, other)?)
    }

    #[inline(never)]
    fn rope_equals(&self, cx: &Cx, other: &JsString) -> JsResult<bool> {
        Ok(self.view(cx)?.text() == other.view(cx)?.text())
    }

    #[inline]
    pub fn compare(&self, cx: &Cx, other: &JsString) -> JsResult<Ordering> {
        Ok(self.view(cx)?.text().cmp(&other.view(cx)?.text()))
    }
}
