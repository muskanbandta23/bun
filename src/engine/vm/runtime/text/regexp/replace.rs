//! What `Symbol.replace` does with a pristine object and a string.

use core::ops::ControlFlow::Continue;

use bun_engine_regexp::{Mode, Template};
use bun_engine_text::{StringBuilder, Text};

use super::global_search::{Matches, try_push};
use super::splice::{Splicer, splice_all, splice_flat};
use crate::{
    CaptureBuffer, Captures, Cx, Gc, JsResult, JsString, RegExp, RegExpObject, StringView,
};

fn substitute(
    template: &Template<'_>,
    out: &mut StringBuilder,
    subject: Text<'_>,
    it: Captures<'_>,
) {
    template.append(out, subject, it.whole().range(), &it.offsets()[2..]);
}

impl RegExpObject {
    pub fn replace_with_string(
        &self,
        cx: &mut Cx,
        string: Gc<JsString>,
        replacement: Gc<JsString>,
    ) -> JsResult<Gc<JsString>> {
        let (subject, template) = (string.view(cx)?, replacement.view(cx)?);
        let (regexp, template) = (self.regexp(), template.text());
        let is_literal = !template.contains_unit(b'$'.into());
        let result = if regexp.is_global() {
            self.set_last_index(cx, 0)?;
            match is_literal {
                true => regexp.replace_all_literally(cx, string, &subject, replacement)?,
                false => regexp.substitute_all(cx, string, &subject, template)?,
            }
        } else if is_literal {
            let found = self.with_last_index_updated(
                cx,
                string,
                |cx, regexp, start| regexp.find(cx, subject.text(), start),
                |found| found.end,
            )?;
            let Some(found) = found else {
                return Ok(string);
            };
            cx.global().record_match(&regexp, string, found);
            Some(splice_flat(cx, &subject, found, template)?)
        } else {
            self.substitute_first(cx, string, &subject, template)?
        };
        Ok(result.unwrap_or(string))
    }

    /// Updates `lastIndex` and the record of the last match.
    pub(super) fn exec_into<'a>(
        &self,
        cx: &mut Cx,
        string: Gc<JsString>,
        subject: &StringView,
        buffer: &'a mut CaptureBuffer,
    ) -> JsResult<Option<Captures<'a>>> {
        let captures = self.with_last_index_updated(
            cx,
            string,
            |cx, regexp, start| regexp.exec(cx, subject.text(), start, buffer),
            |captures| captures.whole().end,
        )?;
        if let Some(captures) = captures {
            (cx.global()).record_match(&self.regexp(), string, captures.whole());
        }
        Ok(captures)
    }

    fn substitute_first(
        &self,
        cx: &mut Cx,
        string: Gc<JsString>,
        subject: &StringView,
        template: Text<'_>,
    ) -> JsResult<Option<Gc<JsString>>> {
        let (regexp, mut buffer) = (self.regexp(), CaptureBuffer::default());
        let Some(captures) = self.exec_into(cx, string, subject, &mut buffer)? else {
            return Ok(None);
        };
        let (subject, found) = (subject.text(), captures.whole());
        let mut out = StringBuilder::with_capacity(subject.len() + template.len());
        let analysis = regexp.analyze(cx, subject, Mode::Captures)?;
        let template = Template::new(template, regexp.group_count() as u16, &analysis.names);
        out.push_text(subject.slice(0..found.start as usize));
        substitute(&cx.or_out_of_memory(template)?, &mut out, subject, captures);
        out.push_text(subject.slice_from(found.end as usize));
        cx.finish_string(out).map(Some)
    }
}

impl RegExp {
    fn replace_all_literally(
        &self,
        cx: &Cx,
        string: Gc<JsString>,
        subject: &StringView,
        replacement: Gc<JsString>,
    ) -> JsResult<Option<Gc<JsString>>> {
        let mut matches = Matches::new();
        let result = self.for_each_match(cx, subject.text(), 0, |found| {
            try_push(cx, &mut matches, found).map(Continue)
        });
        let Some(&last) = matches.last() else {
            return result.map(|_| None);
        };
        cx.global().record_match(self, string, last);
        result.and_then(|_| splice_all(cx, subject, &matches, replacement).map(Some))
    }

    fn substitute_all(
        &self,
        cx: &Cx,
        string: Gc<JsString>,
        subject: &StringView,
        template: Text<'_>,
    ) -> JsResult<Option<Gc<JsString>>> {
        let analysis = self.analyze(cx, subject.text(), Mode::Captures)?;
        let (mut splicer, mut parsed, mut last) = (Splicer::new(subject), None, None);
        let result = self.for_each_exec(cx, subject.text(), 0, |captures| {
            last = Some(captures.whole());
            let template = match &parsed {
                Some(parsed) => parsed,
                None => {
                    let new = Template::new(template, self.group_count() as u16, &analysis.names);
                    parsed.insert(cx.or_out_of_memory(new)?)
                }
            };
            let append = |out: &mut _| substitute(template, out, subject.text(), captures);
            (splicer.replace_by_appending(cx, captures.whole(), append)).map(Continue)
        });
        if let Some(last) = last {
            cx.global().record_match(self, string, last);
        }
        result.and_then(|_| splicer.finish(cx))
    }
}
