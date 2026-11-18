use crate::{
    Attributes, CellType, Cx, DontEnumMode, JsObject, JsResult, PropertyDescriptor,
    PropertyNameList, PropertyNameMode, ShapeFlags, TransitionKind, TypeFlags, Vm,
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

    /// Whether one transition can do what defining each property again would.
    fn can_restrict_at_once(&self, cx: &mut Cx) -> JsResult<bool> {
        if matches!(self.cell_type(), CellType::FinalObject | CellType::Array) {
            return Ok(true);
        }
        let exotic = TypeFlags::OVERRIDES_GET_OWN_PROPERTY_SLOT
            | TypeFlags::OVERRIDES_OWN_PROPERTY_NAMES
            | TypeFlags::OVERRIDES_OWN_SPECIAL_PROPERTY_NAMES;
        let class = self.class();
        if !class.has_properties_of_arrays()
            && (self.shape().type_flags().intersects(exotic)
                || !class.has_ordinary_define_own_property()
                || !class.has_ordinary_prevent_extensions())
        {
            return Ok(false);
        }
        self.reify_all_static_properties(cx)?;
        let properties = self.shape().properties();
        Ok(!(properties.iter()).any(|(_, _, it)| it.intersects(Attributes::CUSTOM)))
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
        if self.can_restrict_at_once(cx)? {
            self.restrict_ordinary(cx, level);
            return Ok(true);
        }
        if !self.prevent_extensions(cx)? {
            return Ok(false);
        }
        let kinds = PropertyNameMode::StringsAndSymbols;
        let mut names = PropertyNameList::new(kinds).including_private_names();
        self.own_property_names(cx, &mut names, DontEnumMode::Include)?;
        for name in names.iter().filter(|it| !it.name().is_private_element()) {
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

    /// `Object.seal`
    pub fn seal_or_throw(&self, cx: &mut Cx) -> JsResult<()> {
        if self.set_integrity_level(cx, IntegrityLevel::Sealed)? {
            return Ok(());
        }
        Err(cx.throw_type_error("Unable to prevent extension in Object.seal"))
    }

    /// `Object.freeze`
    pub fn freeze_or_throw(&self, cx: &mut Cx) -> JsResult<()> {
        if self.set_integrity_level(cx, IntegrityLevel::Frozen)? {
            return Ok(());
        }
        Err(cx.throw_type_error("Unable to prevent extension in Object.freeze"))
    }

    /// For an ordinary object or an array that is not extensible.
    fn is_restricted(&self, level: IntegrityLevel) -> bool {
        let shape = self.shape();
        let (is_frozen, elements) = match level {
            IntegrityLevel::Sealed => (false, ShapeFlags::ELEMENTS_ARE_SEALED),
            IntegrityLevel::Frozen => (true, ShapeFlags::ELEMENTS_ARE_FROZEN),
        };
        if is_frozen
            && self.indexing_mode().is_array()
            && !shape.has(ShapeFlags::LENGTH_IS_READ_ONLY)
        {
            return false;
        }
        let writable = |it: Attributes| !it.intersects(Attributes::REJECTS_PLAIN_STORE);
        let are_elements_restricted = match self.sparse_elements() {
            Some(sparse) => (sparse.entries().iter())
                .map(|entry| entry.1.attributes)
                .all(|it| !it.is_configurable() && !(is_frozen && writable(it))),
            None => shape.has(elements) || self.count_dense_elements() == 0,
        };
        are_elements_restricted
            && (shape.transition_kind() == TransitionKind::Freeze
                || if is_frozen {
                    shape.is_frozen()
                } else {
                    shape.is_sealed()
                })
    }

    /// TestIntegrityLevel
    pub fn test_integrity_level(&self, cx: &mut Cx, level: IntegrityLevel) -> JsResult<bool> {
        if self.is_extensible(cx)? {
            return Ok(false);
        }
        if matches!(self.cell_type(), CellType::FinalObject | CellType::Array) {
            return Ok(self.is_restricted(level));
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
