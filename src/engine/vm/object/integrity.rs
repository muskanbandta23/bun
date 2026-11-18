use crate::{
    Attributes, CellType, Cx, DontEnumMode, JsObject, JsResult, PropertyDescriptor,
    PropertyNameMode, ShapeFlags, TransitionKind, Vm,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IntegrityLevel {
    Sealed,
    Frozen,
}

impl JsObject {
    pub fn ordinary_prevent_extensions(&self, vm: &Vm) {
        if self.shape().is_extensible() {
            self.non_property_transition(vm, TransitionKind::PreventExtensions);
        }
    }

    fn has_only_ordinary_properties(&self) -> bool {
        matches!(
            self.cell_type(),
            CellType::FinalObject | CellType::Array | CellType::DerivedArray
        )
    }

    /// For an object whose own properties are all in its shape and its elements.
    pub fn restrict_ordinary(&self, vm: &Vm, level: IntegrityLevel) {
        let shape = self.shape();
        let is_frozen = level == IntegrityLevel::Frozen;
        if let Some(sparse) = self.sparse_elements() {
            for &(index, entry) in &sparse.entries() {
                let mut attributes = entry.attributes | Attributes::DONT_DELETE;
                if is_frozen && !attributes.contains(Attributes::ACCESSOR) {
                    attributes |= Attributes::READ_ONLY;
                }
                sparse.insert(index, entry.value, attributes);
            }
        }
        if is_frozen {
            if self.public_length() != 0 {
                self.notify_presence_of_indexed_accessors(vm);
            }
            if !shape.has(ShapeFlags::ELEMENTS_ARE_FROZEN) {
                self.non_property_transition(vm, TransitionKind::Freeze);
            }
        } else if !shape.has(ShapeFlags::ELEMENTS_ARE_SEALED) {
            self.non_property_transition(vm, TransitionKind::Seal);
        }
        self.did_change_as_prototype(vm, &shape);
    }

    /// SetIntegrityLevel
    pub fn set_integrity_level(&self, cx: &mut Cx, level: IntegrityLevel) -> JsResult<bool> {
        if self.has_only_ordinary_properties() {
            self.restrict_ordinary(cx, level);
            return Ok(true);
        }
        if !self.prevent_extensions(cx)? {
            return Ok(false);
        }
        let kinds = PropertyNameMode::StringsAndSymbols;
        for name in self.own_keys(cx, kinds, DontEnumMode::Include)?.iter() {
            let mut descriptor = PropertyDescriptor::default();
            descriptor.set_configurable(false);
            if level == IntegrityLevel::Frozen {
                match self.get_own_property_descriptor(cx, name.name())? {
                    Some(current) if current.is_data() => descriptor.set_writable(false),
                    Some(_) => {}
                    None => continue,
                }
            }
            self.define_property_or_throw(cx, name.name(), &descriptor)?;
        }
        Ok(true)
    }

    /// TestIntegrityLevel
    pub fn test_integrity_level(&self, cx: &mut Cx, level: IntegrityLevel) -> JsResult<bool> {
        if self.is_extensible(cx)? {
            return Ok(false);
        }
        if self.cell_type() == CellType::FinalObject && !self.indexing_mode().has_elements() {
            return Ok(match level {
                IntegrityLevel::Sealed => self.shape().is_sealed(),
                IntegrityLevel::Frozen => self.shape().is_frozen(),
            });
        }
        let kinds = PropertyNameMode::StringsAndSymbols;
        for name in self.own_keys(cx, kinds, DontEnumMode::Include)?.iter() {
            if let Some(current) = self.get_own_property_descriptor(cx, name.name())?
                && (current.configurable()
                    || (level == IntegrityLevel::Frozen && current.writable()))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
