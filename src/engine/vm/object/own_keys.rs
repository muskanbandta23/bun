use bun_engine_text::PropertyName;

use crate::{
    CellType, Cx, DontEnumMode, JsObject, JsResult, Properties, PropertyNameList, PropertyNameMode,
    ShapeFlags, TypeFlags,
};

/// The own keys that an object had when it was asked. Those in the table of its shape are not copied.
pub struct OwnKeys {
    first: PropertyNameList,
    rest: Properties,
    mode: DontEnumMode,
    /// Of `rest`.
    has_only_enumerable_strings: bool,
}

impl OwnKeys {
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = PropertyName<'_>> {
        let (mut first, mut rest) = (
            self.first.iter(),
            self.rest.keys(self.first.mode(), self.mode),
        );
        core::iter::from_fn(move || match first.next() {
            Some(name) => Some(name.name()),
            None => rest.as_mut()?.next(),
        })
    }

    pub fn count(&self) -> usize {
        if self.has_only_enumerable_strings && self.first.includes_strings() {
            return self.max_len();
        }
        self.iter().count()
    }

    /// At least as many as there are.
    pub fn max_len(&self) -> usize {
        self.first.len() + self.rest.len()
    }
}

impl JsObject {
    /// `own_keys`, for going through them once.
    pub fn snapshot_own_keys(
        &self,
        cx: &mut Cx,
        kinds: PropertyNameMode,
        mode: DontEnumMode,
    ) -> JsResult<OwnKeys> {
        if (self.shape().type_flags()).contains(TypeFlags::OVERRIDES_OWN_PROPERTY_NAMES) {
            return Ok(OwnKeys {
                first: self.own_keys(cx, kinds, mode)?,
                rest: Properties::NONE,
                mode,
                has_only_enumerable_strings: true,
            });
        }
        let mut indices = PropertyNameList::new(kinds);
        self.own_indexed_property_names(&mut indices, mode);
        if indices.has_overflowed() {
            return Err(cx.throw_out_of_memory());
        }
        self.add_own_non_index_keys(cx, indices, mode)
    }

    /// `own_non_index_property_names`
    pub fn snapshot_own_non_index_keys(
        &self,
        cx: &mut Cx,
        kinds: PropertyNameMode,
        mode: DontEnumMode,
    ) -> JsResult<OwnKeys> {
        self.add_own_non_index_keys(cx, PropertyNameList::new(kinds), mode)
    }

    fn add_own_non_index_keys(
        &self,
        cx: &mut Cx,
        mut first: PropertyNameList,
        mode: DontEnumMode,
    ) -> JsResult<OwnKeys> {
        let shape = self.shape();
        let has_special_names =
            (shape.type_flags()).contains(TypeFlags::OVERRIDES_OWN_SPECIAL_PROPERTY_NAMES);
        let rest = if shape.has_non_reified_static_properties()
            || (has_special_names && self.cell_type() != CellType::Array)
        {
            self.own_non_index_property_names(cx, &mut first, mode)?;
            Properties::NONE
        } else {
            if has_special_names {
                self.own_special_property_names(cx, &mut first, mode)?;
            }
            shape.properties()
        };
        Ok(OwnKeys {
            first,
            rest,
            mode,
            has_only_enumerable_strings: !shape.has(ShapeFlags::PREVENTS_QUICK_ENUMERATION),
        })
    }
}
