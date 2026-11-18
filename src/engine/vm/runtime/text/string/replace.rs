use core::ops::Range;

use bun_engine_text::{StringBuilder, Text};

use crate::{Cx, Gc, JsResult, JsString, StringSplicer, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReplaceMode {
    First,
    All,
}

impl ReplaceMode {
    #[inline(always)]
    fn limit(self) -> usize {
        match self {
            ReplaceMode::First => 1,
            ReplaceMode::All => usize::MAX,
        }
    }
}

const DOLLAR: u16 = b'$' as u16;

/// GetSubstitution without captures.
fn append_substitution(
    out: &mut StringBuilder,
    template: Text<'_>,
    first_dollar: usize,
    subject: Text<'_>,
    matched: Range<usize>,
) {
    let (mut done, mut dollar) = (0, Some(first_dollar));
    while let Some(at) = dollar {
        let value = match template.get(at + 1).and_then(|it| u8::try_from(it).ok()) {
            Some(b'$') => Some(Text::from_ascii("$")),
            Some(b'&') => Some(subject.slice(matched.clone())),
            Some(b'`') => Some(subject.slice(0..matched.start)),
            Some(b'\'') => Some(subject.slice_from(matched.end)),
            _ => None,
        };
        if let Some(value) = value {
            out.push_text(template.slice(done..at));
            out.push_text(value);
            done = at + 2;
        }
        dollar = template.find_unit(DOLLAR, done.max(at + 1));
    }
    out.push_text(template.slice_from(done));
}

impl JsString {
    /// `String.prototype.replace` and `replaceAll`, for a search that is a string.
    #[inline]
    pub fn replace(
        &self,
        cx: &mut Cx,
        mode: ReplaceMode,
        search: Gc<JsString>,
        replacement: Value,
    ) -> JsResult<Gc<JsString>> {
        if !replacement.is_string() && replacement.is_callable() {
            return self.replace_by_calling(cx, mode, search, replacement);
        }
        let template = replacement.to_string(cx)?;
        self.replace_by_template(cx, mode, &search, template)
    }

    /// `$$`, `$&`, `` $` `` and `$'` in `template` are expanded.
    #[inline]
    pub fn replace_by_template(
        &self,
        cx: &Cx,
        mode: ReplaceMode,
        search: &JsString,
        template: Gc<JsString>,
    ) -> JsResult<Gc<JsString>> {
        let view = template.view(cx)?;
        match (view.text().find_unit(DOLLAR, 0), mode) {
            (None, ReplaceMode::First) => self.replace_first_literally(cx, search, template),
            (None, ReplaceMode::All) => self.replace_all_literally(cx, search, &template),
            (Some(dollar), _) => self.replace_expanding(cx, mode, search, view.text(), dollar),
        }
    }

    pub fn replace_first_literally(
        &self,
        cx: &Cx,
        search: &JsString,
        replacement: Gc<JsString>,
    ) -> JsResult<Gc<JsString>> {
        let search = search.view(cx)?;
        let search = search.text();
        if search.len() == 1
            && self.is_worth_walking()
            && let Some(result) = self.replace_unit_in_fibers(cx, search.at(0), replacement)?
        {
            return Ok(result);
        }
        let view = self.view(cx)?;
        match view.text().find(search, 0) {
            Some(start) => view.replaced(cx, start..start + search.len(), replacement),
            None => Ok(self.into()),
        }
    }

    pub fn replace_all_literally(
        &self,
        cx: &Cx,
        search: &JsString,
        replacement: &JsString,
    ) -> JsResult<Gc<JsString>> {
        let (search, replacement) = (search.view(cx)?, replacement.view(cx)?);
        let (search, replacement) = (search.text(), replacement.text());
        self.converted(cx, |text| match (search.len(), replacement.len()) {
            (1, 1) => text.replace_unit(search.at(0), replacement.at(0)),
            _ => text.replace_all(search, replacement),
        })
    }

    #[inline(never)]
    fn replace_expanding(
        &self,
        cx: &Cx,
        mode: ReplaceMode,
        search: &JsString,
        template: Text<'_>,
        dollar: usize,
    ) -> JsResult<Gc<JsString>> {
        let (view, search) = (self.view(cx)?, search.view(cx)?);
        let mut splicer = StringSplicer::new(&view);
        for start in view.text().matches(search.text()).take(mode.limit()) {
            let range = start..start + search.text().len();
            splicer.replace_with(range.clone(), |out| {
                append_substitution(out, template, dollar, view.text(), range);
            });
        }
        Ok(splicer.finish(cx)?.unwrap_or(self.into()))
    }

    #[inline(never)]
    fn replace_by_calling(
        &self,
        cx: &mut Cx,
        mode: ReplaceMode,
        search: Gc<JsString>,
        function: Value,
    ) -> JsResult<Gc<JsString>> {
        let (view, search_view) = (self.view(cx)?, search.view(cx)?);
        let mut splicer = StringSplicer::new(&view);
        for start in view.text().matches(search_view.text()).take(mode.limit()) {
            let arguments = [search.into(), start.into(), self.into()];
            let replacement = cx.call(function, Value::UNDEFINED, &arguments)?;
            let range = start..start + search_view.text().len();
            splicer.replace(range, replacement.to_string(cx)?);
        }
        Ok(splicer.finish(cx)?.unwrap_or(self.into()))
    }
}
