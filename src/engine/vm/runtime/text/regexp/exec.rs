use bun_engine_text::{Text, names};

use crate::{
    CaptureBuffer, Cx, Gc, JsArray, JsObject, JsResult, JsString, Match, RegExp, RegExpObject,
    Value,
};

/// AdvanceStringIndex
#[inline]
pub fn advance_string_index(text: Text<'_>, index: u64, is_unicode_aware: bool) -> u64 {
    let is_pair = is_unicode_aware
        && index < text.len() as u64
        && text.code_point_at(index as usize) > 0xffff;
    index + 1 + u64::from(is_pair)
}

impl RegExp {
    #[inline(always)]
    fn exec_to_array(
        &self,
        cx: &Cx,
        string: Gc<JsString>,
        start: u32,
    ) -> JsResult<Option<(Gc<JsArray>, Match)>> {
        let (subject, mut buffer) = (string.view(cx)?, CaptureBuffer::default());
        let Some(captures) = self.exec(cx, subject.text(), start, &mut buffer)? else {
            return Ok(None);
        };
        let array = self.match_array(cx, string, &subject, captures)?;
        Ok(Some((array, captures.whole())))
    }

    #[inline(always)]
    fn find_and_record(
        &self,
        cx: &Cx,
        string: Gc<JsString>,
        start: u32,
    ) -> JsResult<Option<Match>> {
        let found = self.find(cx, string.view(cx)?.text(), start)?;
        if let Some(found) = found {
            cx.global().record_match(self, string, found);
        }
        Ok(found)
    }

    /// What `Symbol.search` returns.
    #[inline]
    pub fn search(&self, cx: &Cx, string: Gc<JsString>) -> JsResult<i32> {
        let found = self.find_and_record(cx, string, 0)?;
        Ok(found.map_or(-1, |found| found.start as i32))
    }
}

impl RegExpObject {
    /// The steps of RegExpBuiltinExec around `search`, which gets the pattern and where to start.
    #[inline(always)]
    pub(super) fn with_last_index_updated<T>(
        &self,
        cx: &mut Cx,
        string: Gc<JsString>,
        search: impl FnOnce(&Cx, &RegExp, u32) -> JsResult<Option<T>>,
        end_of: impl FnOnce(&T) -> u32,
    ) -> JsResult<Option<T>> {
        let last_index = self.last_index_within(cx, string.len())?;
        let regexp = self.regexp();
        if !regexp.uses_last_index() {
            return search(cx, &regexp, 0);
        }
        let result = match last_index {
            Some(start) => search(cx, &regexp, start)?,
            None => None,
        };
        self.set_last_index(cx, result.as_ref().map_or(0, end_of).into())?;
        Ok(result)
    }

    #[inline(always)]
    pub(super) fn exec_with_range(
        &self,
        cx: &mut Cx,
        string: Gc<JsString>,
    ) -> JsResult<Option<(Gc<JsArray>, Match)>> {
        let result = self.with_last_index_updated(
            cx,
            string,
            |cx, regexp, start| {
                let result = regexp.exec_to_array(cx, string, start)?;
                Ok(result.map(|result| (result, Gc::from(regexp))))
            },
            |((_, found), _)| found.end,
        )?;
        Ok(result.map(|(result, regexp)| {
            cx.global().record_match(&regexp, string, result.1);
            result
        }))
    }

    /// RegExpBuiltinExec
    #[inline]
    pub fn exec(&self, cx: &mut Cx, string: Gc<JsString>) -> JsResult<Value> {
        let result = self.exec_with_range(cx, string)?;
        Ok(result.map_or(Value::NULL, |(array, _)| array.into()))
    }

    /// The same, without the array.
    #[inline]
    pub fn test(&self, cx: &mut Cx, string: Gc<JsString>) -> JsResult<bool> {
        let found = self.with_last_index_updated(
            cx,
            string,
            |cx, regexp, start| regexp.find_and_record(cx, string, start),
            |found| found.end,
        )?;
        Ok(found.is_some())
    }

    /// What `Symbol.match` does with a pristine object.
    #[inline]
    pub fn match_pristine(&self, cx: &mut Cx, string: Gc<JsString>) -> JsResult<Value> {
        let regexp = self.regexp();
        if !regexp.is_global() {
            return self.exec(cx, string);
        }
        self.set_last_index(cx, 0)?;
        let matches = regexp.match_all(cx, string)?;
        Ok(matches.map_or(Value::NULL, Into::into))
    }
}

/// RegExpExec. `None` is null.
pub fn regexp_exec(
    cx: &mut Cx,
    regexp: Gc<JsObject>,
    string: Gc<JsString>,
) -> JsResult<Option<Gc<JsObject>>> {
    let (exec, builtin) = (regexp.get(cx, names::exec)?, cx.regexp_prototype_exec());
    let result = if exec != builtin.into() && exec.is_callable() {
        cx.call(exec, regexp.into(), &[string.into()])?
    } else if let Some(regexp) = regexp.downcast::<RegExpObject>() {
        regexp.exec(cx, string)?
    } else {
        cx.call(builtin.into(), regexp.into(), &[string.into()])?
    };
    if !result.is_null() && !result.is_object() {
        return Err(cx.throw_type_error("The result of RegExp exec must be null or an object"));
    }
    Ok(result.as_object())
}
