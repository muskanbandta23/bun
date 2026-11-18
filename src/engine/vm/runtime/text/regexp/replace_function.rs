//! What `Symbol.replace` does with a pristine object and a function.

use core::ops::ControlFlow::Continue;

use bun_engine_regexp::Mode;
use bun_engine_text::StringImpl;
use smallvec::SmallVec;

use super::global_search::check_termination;
use super::splice::Splicer;
use crate::{
    CallData, CaptureBuffer, Captures, Cx, Gc, ImmutableArray, JsResult, JsString, Match, RegExp,
    RegExpObject, StringView, Value, ValueVec, options,
};

/// What `RegExp.lastMatch` and its kin tell the function.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MatchOnRecord {
    Last,
    /// The one that it is called for, unless the string is long.
    Current,
}

struct Replacer<'a> {
    function: Value,
    string: Gc<JsString>,
    subject: &'a StringView,
    arguments: ValueVec,
    splicer: Splicer<'a>,
}

impl Replacer<'_> {
    #[inline]
    fn replace(&mut self, cx: &mut Cx, regexp: &RegExp, captures: Captures<'_>) -> JsResult<()> {
        let arguments = &mut self.arguments;
        arguments.clear();
        cx.or_out_of_memory(arguments.try_reserve(captures.count() + 3))?;
        for index in 0..captures.count() {
            arguments.push(captures.value(cx, self.subject, index));
        }
        arguments.push(captures.whole().start.into());
        arguments.push(self.string.into());
        if regexp.has_named_groups() {
            let groups = regexp.groups(cx, arguments);
            arguments.push(groups);
        }
        self.call(cx, captures.whole())
    }

    #[inline(always)]
    fn call(&mut self, cx: &mut Cx, found: Match) -> JsResult<()> {
        let result = cx.call(self.function, Value::UNDEFINED, &self.arguments)?;
        let replacement = result.to_string(cx)?;
        self.splicer.replace(cx, found, replacement)
    }

    fn replace_while_searching(&mut self, cx: &mut Cx, regexp: &RegExp) -> JsResult<()> {
        let (subject, mut buffer) = (self.subject.text(), CaptureBuffer::default());
        let mut start = 0;
        while let Some(captures) = regexp.exec(cx, subject, start, &mut buffer)? {
            (cx.global()).record_match(regexp, self.string, captures.whole());
            self.replace(cx, regexp, captures)?;
            start = regexp.start_after(subject, captures.whole());
            check_termination(cx)?;
        }
        Ok(())
    }

    fn replace_after_searching(&mut self, cx: &mut Cx, regexp: &RegExp) -> JsResult<()> {
        let (mut offsets, mut last) = (SmallVec::<[u32; 64]>::new(), None);
        let result = regexp.for_each_exec(cx, self.subject.text(), 0, |captures| {
            last = Some(captures.whole());
            if offsets.try_reserve(captures.offsets().len()).is_err() {
                return Err(cx.throw_out_of_memory());
            }
            offsets.extend_from_slice(captures.offsets());
            Ok(Continue(()))
        });
        if let Some(last) = last {
            cx.global().record_match(regexp, self.string, last);
        }
        let _ = result?;
        for offsets in offsets.chunks_exact((regexp.group_count() + 1) * 2) {
            self.replace(cx, regexp, Captures::new(offsets))?;
        }
        Ok(())
    }

    /// For each match the arguments that come before the string.
    fn leading_arguments(
        &self,
        cx: &Cx,
        regexp: &RegExp,
        atom: &StringImpl,
    ) -> JsResult<Option<Gc<ImmutableArray>>> {
        let cache = &cx.regexps().replace_cache;
        if let Some((all, Some(last))) = cache.get(cx, atom, regexp) {
            cx.global().record_match(regexp, self.string, last);
            return Ok(Some(all));
        }
        let (mut values, mut last) = (ValueVec::new(), None);
        let result = regexp.for_each_exec(cx, self.subject.text(), 0, |captures| {
            last = Some(captures.whole());
            cx.or_out_of_memory(values.try_reserve(captures.count() + 1))?;
            for index in 0..captures.count() {
                values.push(captures.value(cx, self.subject, index));
            }
            values.push(captures.whole().start.into());
            Ok(Continue(()))
        });
        let Some(found) = last else {
            return result.map(|_| None);
        };
        cx.global().record_match(regexp, self.string, found);
        let _ = result?;
        let all = cx.or_out_of_memory(ImmutableArray::try_from_values(cx, &values))?;
        cache.insert(atom, regexp, all, last);
        Ok(Some(all))
    }

    fn replace_from_cache(
        &mut self,
        cx: &mut Cx,
        regexp: &RegExp,
        atom: &StringImpl,
    ) -> JsResult<()> {
        let Some(all) = self.leading_arguments(cx, regexp, atom)? else {
            return Ok(());
        };
        let values = all.values().unwrap_or_default();
        for leading in values.chunks_exact(regexp.group_count() + 2) {
            let start = leading[leading.len() - 1].to_bits() as u32;
            let end = start + leading[0].cast::<JsString>().len();
            self.arguments.clear();
            cx.or_out_of_memory(self.arguments.try_extend_from_slice(leading))?;
            cx.or_out_of_memory(self.arguments.try_push(self.string.into()))?;
            self.call(cx, Match { start, end })?;
        }
        all.keep_alive();
        Ok(())
    }
}

impl RegExpObject {
    pub fn replace_with_function(
        &self,
        cx: &mut Cx,
        string: Gc<JsString>,
        function: Value,
        on_record: MatchOnRecord,
    ) -> JsResult<Gc<JsString>> {
        let (regexp, subject) = (self.regexp(), string.view(cx)?);
        let mut replacer = Replacer {
            function,
            string,
            subject: &subject,
            arguments: ValueVec::new(),
            splicer: Splicer::new(&subject),
        };
        if regexp.is_global() {
            self.set_last_index(cx, 0)?;
            regexp.analyze(cx, subject.text(), Mode::Captures)?;
            let is_long = subject.text().len() >= options().threshold_for_replace_cache
                && !regexp.has_named_groups()
                && !CallData::for_call(function).is_native();
            let atom = string.try_resolved().filter(|it| is_long && it.is_atom());
            if let Some(atom) = atom {
                replacer.replace_from_cache(cx, &regexp, &atom)?;
            } else if is_long || on_record == MatchOnRecord::Last {
                replacer.replace_after_searching(cx, &regexp)?;
            } else {
                replacer.replace_while_searching(cx, &regexp)?;
            }
        } else {
            let mut buffer = CaptureBuffer::default();
            if let Some(captures) = self.exec_into(cx, string, &subject, &mut buffer)? {
                replacer.replace(cx, &regexp, captures)?;
            }
        }
        Ok(replacer.splicer.finish(cx)?.unwrap_or(string))
    }
}
