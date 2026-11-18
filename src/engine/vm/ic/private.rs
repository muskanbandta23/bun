//! A class that is evaluated again has other names. The inline case is for one of them, and those in the list say which
//! they are for.

use super::load::prepare;
use super::{AccessCase, AccessKind, Key, store};
use crate::code::metadata::{GetPrivateName, PutPrivateName};
use crate::{
    CodeBlock, Cx, Gc, InternalMethod, JsObject, JsResult, JsSymbol, PropertySlot, PutPropertySlot,
    Value,
};

impl GetPrivateName {
    /// For when no case applies.
    pub fn get_slow(
        &self,
        cx: &Cx,
        owner: &CodeBlock,
        base: Value,
        symbol: Gc<JsSymbol>,
    ) -> JsResult<Value> {
        let mut slot = PropertySlot::new(base, InternalMethod::GetOwnProperty);
        let value = base.get_private_field(cx, symbol.anchor().name(), &mut slot)?;
        if let Some(object) = slot.base()
            && (self.cache.shape().is_none() || self.cache.list().should_cache())
            && prepare(cx, &object, false).is_ok()
        {
            let case = AccessCase::new(AccessKind::LoadOwn, object.shape_id())
                .with_offset(slot.cached_offset());
            if self.cache.shape().is_none() {
                self.cache.set_inline_own(&case);
                self.name.set(owner, &symbol);
            } else {
                let key = Some(symbol.anchor().name());
                (self.cache.list()).add(cx, owner, case, key, AccessKind::Generic);
            }
        }
        Ok(value)
    }
}

impl PutPrivateName {
    fn add(&self, cx: &Cx, owner: &CodeBlock, case: AccessCase, symbol: Gc<JsSymbol>) {
        let Err(case) = self.cache.try_set_inline(owner, case) else {
            return self.name.set(owner, &symbol);
        };
        let inline_symbol = self.name.get().map(Gc::cast::<JsSymbol>);
        let inline_name = inline_symbol.as_ref().map(|it| it.anchor().name());
        let cases = self.cache.ensure_cases(cx, owner, inline_name);
        cases.add(
            cx,
            owner,
            case,
            Some(symbol.anchor().name()),
            AccessKind::Generic,
        );
    }

    #[inline(always)]
    fn store(
        &self,
        cx: &mut Cx,
        owner: &CodeBlock,
        base: Value,
        symbol: Gc<JsSymbol>,
        value: Value,
        uncached: impl FnOnce(&Cx, &mut PutPropertySlot) -> JsResult<()>,
    ) -> JsResult<()> {
        let mut slot = PutPropertySlot::with_context(base, true, crate::PutContext::PutById);
        let Some(object) = base.as_object() else {
            return uncached(cx, &mut slot);
        };
        let (old, name) = (object.shape(), symbol.anchor().name());
        if let Some(case) = (self.cache.cases()).and_then(|it| it.find(old.id(), name.address()))
            && let Some(result) = store::execute(cx, case, object, name, value, true)
        {
            return result;
        }
        uncached(cx, &mut slot)?;
        if self.cache.cases().is_none_or(|cases| cases.should_cache()) {
            match store::plan(cx, object, old, name, &slot, false) {
                Ok(case) => self.add(cx, owner, case, symbol),
                Err(super::load::Plan::Fail) => {
                    if let Some(cases) = self.cache.cases() {
                        cases.did_fail_to_cache(cx, owner, Key::Variable);
                    }
                }
                Err(_) => {}
            }
        }
        Ok(())
    }

    pub fn set_slow(
        &self,
        cx: &mut Cx,
        owner: &CodeBlock,
        base: Value,
        symbol: Gc<JsSymbol>,
        value: Value,
    ) -> JsResult<()> {
        self.store(cx, owner, base, symbol, value, |cx, slot| {
            base.set_private_field(cx, symbol.anchor().name(), value, slot)
        })
    }

    pub fn define_slow(
        &self,
        cx: &mut Cx,
        owner: &CodeBlock,
        base: Gc<JsObject>,
        symbol: Gc<JsSymbol>,
        value: Value,
    ) -> JsResult<()> {
        self.store(cx, owner, base.into(), symbol, value, |cx, slot| {
            base.define_private_field(cx, symbol.anchor().name(), value, slot)
        })
    }

    pub fn set_brand_slow(
        &self,
        cx: &mut Cx,
        owner: &CodeBlock,
        base: Gc<JsObject>,
        brand: Gc<JsSymbol>,
    ) -> JsResult<()> {
        let value = JsObject::PRIVATE_BRAND_VALUE;
        self.store(cx, owner, base.into(), brand, value, |cx, slot| {
            base.set_private_brand(cx, brand.anchor().name(), slot)
        })
    }

    /// A case says only that the shape has the brand, which nothing takes away.
    pub fn check_brand_slow(
        &self,
        cx: &Cx,
        owner: &CodeBlock,
        base: Value,
        brand: Gc<JsSymbol>,
    ) -> JsResult<()> {
        let mut slot = PropertySlot::new(base, InternalMethod::GetOwnProperty);
        base.check_private_brand(cx, brand.anchor().name(), &mut slot)?;
        if let Some(object) = slot.base()
            && (self.cache.cases()).is_some_and(|cases| cases.should_cache())
        {
            let case = AccessCase::new(AccessKind::HasOwn, object.shape_id());
            self.add(cx, owner, case, brand);
        }
        Ok(())
    }
}
