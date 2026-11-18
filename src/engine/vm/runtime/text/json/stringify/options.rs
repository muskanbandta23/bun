use bun_engine_text::{Identifier, Text};

use crate::{
    Cx, JsResult, NumberObject, PropertyNameList, PropertyNameMode, StringObject, Value, is_array,
};

pub(super) enum Replacer {
    None,
    Function(Value),
    /// The names of the properties to write.
    List(Vec<Identifier>),
}

impl Replacer {
    #[inline]
    pub(super) fn new(cx: &mut Cx, replacer: Value) -> JsResult<Replacer> {
        if !replacer.is_object() {
            Ok(Replacer::None)
        } else if replacer.is_callable() {
            Ok(Replacer::Function(replacer))
        } else if is_array(cx, replacer)? {
            Self::list(cx, replacer)
        } else {
            Ok(Replacer::None)
        }
    }

    fn list(cx: &mut Cx, array: Value) -> JsResult<Replacer> {
        let array = array.to_object(cx)?;
        let mut names = PropertyNameList::new(PropertyNameMode::Strings);
        for index in 0..array.length_of_array_like(cx)? {
            cx.check_traps()?;
            let item = array.get_index(cx, index)?;
            if item.is_string()
                || item.is_number()
                || item.is::<StringObject>()
                || item.is::<NumberObject>()
            {
                names.add(item.to_string(cx)?.to_property_key(cx)?.name());
            }
        }
        Ok(Replacer::List(names.into_vec()))
    }
}

/// One step of indentation.
#[derive(Default)]
pub(super) struct Gap {
    units: [u16; Gap::LONGEST],
    length: usize,
}

impl Gap {
    const LONGEST: usize = 10;

    #[inline]
    pub(super) fn new(cx: &mut Cx, space: Value) -> JsResult<Gap> {
        if space.is_undefined() {
            return Ok(Gap::default());
        }
        Self::from_space(cx, space)
    }

    fn from_space(cx: &mut Cx, mut space: Value) -> JsResult<Gap> {
        if space.is::<NumberObject>() {
            space = space.to_number(cx)?.into();
        } else if space.is::<StringObject>() {
            space = space.to_string(cx)?.into();
        }
        let mut gap = Gap::default();
        if let Some(count) = space.as_number() {
            gap.length = (count as usize).min(Self::LONGEST);
            gap.units.fill(b' '.into());
        } else if let Some(string) = space.as_string() {
            let view = string.view(cx)?;
            gap.length = view.text().len().min(Self::LONGEST);
            for (to, from) in gap.units.iter_mut().zip(view.text().code_units()) {
                *to = from;
            }
        }
        Ok(gap)
    }

    #[inline]
    pub(super) fn is_empty(&self) -> bool {
        self.length == 0
    }

    pub(super) fn text(&self) -> Text<'_> {
        Text::Utf16(&self.units[..self.length])
    }
}
