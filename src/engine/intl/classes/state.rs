use core::cell::RefCell;
use std::rc::Rc;

use bun_engine_text::Text;
use bun_engine_vm::{ErrorType, JsCell, Thrown, TypeFlags};

use super::prelude::*;
use crate::{Cache, Error, ErrorKind, Result};

/// The realms that have the language of the host share the cache of the `Vm`.
#[cell(flags = TypeFlags::SHAPE_IS_IMMORTAL)]
struct State {
    base: JsCell,
    cache: Rc<RefCell<Cache>>,
}

pub(super) fn initialize(cx: &Cx) {
    let language = (cx.global().hooks().default_language)();
    let cache = if language.is_empty() {
        cx.extension::<RefCell<Cache>>()
    } else {
        let mut cache = Cache::default();
        cache.set_preferred_languages(&[language.text()]);
        Rc::new(RefCell::new(cache))
    };
    let base = cx.shape_for::<State>();
    let state = cx.allocate::<State>(StateInit { base, cache });
    cx.global().init_intl_state(state.cell());
}

/// In place of the language of the host, for the realm and those that share its cache.
pub fn set_preferred_languages(cx: &Cx, languages: &[Text<'_>]) {
    let state = cx.intl_state().cast::<State>();
    state.cache.borrow_mut().set_preferred_languages(languages);
}

#[inline(always)]
pub(super) fn with_cache<T>(cx: &Cx, call: impl FnOnce(&Cache) -> Result<T>) -> JsResult<T> {
    let state = cx.intl_state().cast::<State>();
    let cache = state.cache.borrow();
    let result = call(&cache);
    let allocated_bytes = cache.take_allocated_bytes();
    drop(cache);
    if allocated_bytes != 0 {
        cx.heap.report_untracked_extra_memory(allocated_bytes);
    }
    result.or_throw(cx)
}

pub(super) trait OrThrow<T> {
    fn or_throw(self, cx: &Cx) -> JsResult<T>;
}

impl<T> OrThrow<T> for Result<T> {
    #[inline(always)]
    fn or_throw(self, cx: &Cx) -> JsResult<T> {
        self.map_err(|error| throw(cx, &error))
    }
}

#[cold]
pub(super) fn throw(cx: &Cx, error: &Error) -> Thrown {
    match error.kind() {
        ErrorKind::Error => cx.throw_error(ErrorType::Error, error.message()),
        ErrorKind::RangeError => cx.throw_range_error(error.message()),
        ErrorKind::TypeError => cx.throw_type_error(error.message()),
        ErrorKind::OutOfMemory => cx.throw_out_of_memory(),
        ErrorKind::Interruption => cx.throw_termination(),
    }
}
