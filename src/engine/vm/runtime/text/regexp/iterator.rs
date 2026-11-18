use core::cell::Cell;
use core::mem::offset_of;

use bun_engine_macros::cell;
use bun_engine_text::names;

use crate::{
    Cx, Gc, JsObject, JsObjectInit, JsResult, JsString, RegExpObject, advance_string_index,
    regexp_exec,
};

#[cell(name = "RegExpStringIterator", cell_type = RegExpStringIterator)]
pub struct RegExpStringIterator {
    base: JsObject,
    regexp: Gc<JsObject>,
    string: Gc<JsString>,
    is_global: bool,
    is_unicode_aware: bool,
    is_done: Cell<bool>,
}

impl RegExpStringIterator {
    pub const OFFSET_OF_IS_DONE: usize = offset_of!(RegExpStringIterator, is_done);

    /// CreateRegExpStringIterator
    pub fn create(
        cx: &Cx,
        regexp: Gc<JsObject>,
        string: Gc<JsString>,
        is_global: bool,
        is_unicode_aware: bool,
    ) -> Gc<RegExpStringIterator> {
        cx.allocate::<RegExpStringIterator>(RegExpStringIteratorInit {
            base: JsObjectInit::new(cx, cx.regexp_string_iterator_shape()),
            regexp,
            string,
            is_global,
            is_unicode_aware,
            is_done: Cell::new(false),
        })
    }

    /// What `Symbol.matchAll` does with an object that is pristine, species included.
    pub fn for_pristine(
        cx: &mut Cx,
        object: &RegExpObject,
        string: Gc<JsString>,
    ) -> JsResult<Gc<RegExpStringIterator>> {
        let regexp = object.regexp();
        let matcher = RegExpObject::create(cx, regexp);
        let last_index = object.last_index().to_length(cx)?;
        matcher.set_last_index(cx, last_index)?;
        let (is_global, is_unicode_aware) = (regexp.is_global(), regexp.flags().is_unicode_aware());
        let matcher = matcher.upcast();
        Ok(Self::create(
            cx,
            matcher,
            string,
            is_global,
            is_unicode_aware,
        ))
    }

    /// `None` at the end.
    pub fn next_match(&self, cx: &mut Cx) -> JsResult<Option<Gc<JsObject>>> {
        if self.is_done.get() {
            return Ok(None);
        }
        let (regexp, string) = (self.regexp, self.string);
        let result = match regexp.downcast::<RegExpObject>() {
            Some(regexp) if regexp.inherits_pristine(cx, &[names::exec]) => regexp
                .exec_with_range(cx, string)?
                .map(|(array, found)| (array.upcast(), found.is_empty())),
            _ => match regexp_exec(cx, regexp, string)? {
                Some(result) if self.is_global => {
                    Some((result, result.get_index(cx, 0)?.to_string(cx)?.is_empty()))
                }
                result => result.map(|result| (result, false)),
            },
        };
        let Some((result, is_empty)) = result else {
            self.is_done.set(true);
            return Ok(None);
        };
        if !self.is_global {
            self.is_done.set(true);
        } else if is_empty {
            advance_last_index(cx, regexp, string, self.is_unicode_aware)?;
        }
        Ok(Some(result))
    }
}

/// After an empty match.
pub fn advance_last_index(
    cx: &mut Cx,
    regexp: Gc<JsObject>,
    string: Gc<JsString>,
    is_unicode_aware: bool,
) -> JsResult<()> {
    let index = regexp.get(cx, names::lastIndex)?.to_length(cx)?;
    let next = advance_string_index(string.view(cx)?.text(), index, is_unicode_aware);
    regexp
        .set(cx, names::lastIndex, next.into(), true)
        .map(drop)
}
