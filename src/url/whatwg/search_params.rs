use bun_engine_text::{AllocError, StringRef, Text};

use crate::form::{self, Pair};

/// The list of a `URLSearchParams` object. A `limit` is as for [`form::parse`].
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct SearchParams(Vec<Pair>);

impl SearchParams {
    /// A leading `?` does not count.
    pub fn parse(init: Text<'_>, limit: usize) -> Result<SearchParams, AllocError> {
        let has_question_mark = init.get(0) == Some(u16::from(b'?'));
        form::parse(init.slice_from(usize::from(has_question_mark)), limit).map(SearchParams)
    }

    #[inline]
    pub fn pairs(&self) -> &[Pair] {
        &self.0
    }

    pub fn append(&mut self, pair: Pair, limit: usize) -> Result<(), AllocError> {
        form::push(&mut self.0, pair, limit)
    }

    pub fn delete(&mut self, name: Text<'_>, value: Option<Text<'_>>) {
        self.0.retain(|pair| !matches(pair, name, value));
    }

    pub fn get_all<'a>(&'a self, name: Text<'a>) -> impl Iterator<Item = &'a StringRef> {
        let named = self.0.iter().filter(move |pair| pair.name.text() == name);
        named.map(|pair| &pair.value)
    }

    pub fn has(&self, name: Text<'_>, value: Option<Text<'_>>) -> bool {
        self.0.iter().any(|pair| matches(pair, name, value))
    }

    pub fn set(&mut self, pair: Pair, limit: usize) -> Result<(), AllocError> {
        let Pair { name, value } = pair;
        let mut value = Some(value);
        self.0.retain_mut(|it| {
            it.name != name || value.take().map(|value| it.value = value).is_some()
        });
        match value {
            Some(value) => self.append(Pair { name, value }, limit),
            None => Ok(()),
        }
    }

    /// By the code units of the names. Pairs of the same name keep their order.
    pub fn sort(&mut self) {
        self.0.sort_by(|a, b| a.name.text().cmp(&b.name.text()));
    }

    pub fn serialize(&self) -> Result<StringRef, AllocError> {
        form::serialize(self.0.iter().map(|it| (it.name.text(), it.value.text())))
    }
}

fn matches(pair: &Pair, name: Text<'_>, value: Option<Text<'_>>) -> bool {
    pair.name.text() == name && value.is_none_or(|value| pair.value.text() == value)
}
