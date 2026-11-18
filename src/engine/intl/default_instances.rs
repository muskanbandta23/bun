use core::cell::{OnceCell, RefCell};

use bun_engine_text::{StringImpl, StringRef};

use crate::{Cache, LanguageTag, Result};

/// What a locale-sensitive method constructs when it gets no options: one for no locale, one for the last locale.
pub(crate) struct DefaultInstances<T> {
    without_locale: OnceCell<T>,
    last: RefCell<Option<(StringRef, T)>>,
}

impl<T> Default for DefaultInstances<T> {
    fn default() -> DefaultInstances<T> {
        DefaultInstances {
            without_locale: OnceCell::new(),
            last: RefCell::new(None),
        }
    }
}

impl<T> DefaultInstances<T> {
    #[inline]
    pub(crate) fn with<R>(
        &self,
        cache: &Cache,
        locale: Option<&StringImpl>,
        construct: impl FnOnce(&[LanguageTag]) -> Result<T>,
        call: impl FnOnce(&T) -> Result<R>,
    ) -> Result<R> {
        let Some(locale) = locale else {
            return call(self.without_locale.get_or_try_init(|| construct(&[]))?);
        };
        let mut last = self.last.borrow_mut();
        match &mut *last {
            Some((written, instance)) if **written == *locale => call(instance),
            other => {
                let instance = construct(&[cache.canonicalize(locale)?])?;
                call(&other.insert((StringRef::retain(locale), instance)).1)
            }
        }
    }
}
