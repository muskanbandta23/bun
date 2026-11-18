use bun_alloc::AllocError;
use bun_engine_text::{StringRef, Text};

use crate::{Cx, Gc, JsResult, JsString};

impl JsString {
    /// `convert` returns `None` if nothing changes.
    #[inline(always)]
    pub fn converted(
        &self,
        cx: &Cx,
        convert: impl FnOnce(Text<'_>) -> Result<Option<StringRef>, AllocError>,
    ) -> JsResult<Gc<JsString>> {
        let converted = cx.or_out_of_memory(convert(self.view(cx)?.text()))?;
        Ok(converted.map_or_else(|| self.into(), |string| Self::new(cx, string)))
    }

    pub fn to_lower_case(&self, cx: &Cx) -> JsResult<Gc<JsString>> {
        self.converted(cx, bun_engine_unicode::to_lower_case)
    }

    pub fn to_upper_case(&self, cx: &Cx) -> JsResult<Gc<JsString>> {
        self.converted(cx, bun_engine_unicode::to_upper_case)
    }

    pub fn is_well_formed(&self, cx: &Cx) -> JsResult<bool> {
        Ok(self.is_latin1() || self.view(cx)?.text().is_well_formed())
    }

    pub fn to_well_formed(&self, cx: &Cx) -> JsResult<Gc<JsString>> {
        if self.is_latin1() {
            return Ok(self.into());
        }
        self.converted(cx, |text| text.to_well_formed())
    }
}
