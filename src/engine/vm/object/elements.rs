//! Properties whose names are array indices.

use core::cell::Cell;

use bun_alloc::AllocError;
use bun_engine_text::Identifier;

use crate::heap::Owned;
use crate::{
    Attributes, CellType, Cx, DeferredFire, DontEnumMode, ElementKind, Field, Gc, GetterSetter,
    GlobalObject, IndexingMode, JsArrayBufferView, JsObject, JsResult, NoGc, PropertyDescriptor,
    PropertyNameList, PropertySlot, ProxyObject, ShapeFlags, SparseElements, SparseEntry, Storage,
    TransitionKind, Value, Vm, errors,
};

pub(super) const MIN_SPARSE_INDEX: u32 = 100_000;
const MIN_SPARSE_DISTANCE: u32 = 1000;
const MIN_DENSITY: u32 = 8;

pub(super) fn is_dense_enough(length: u32, count: usize) -> bool {
    (length / MIN_DENSITY) as usize <= count
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PutDirectIndexMode {
    /// Also on an object that is not extensible.
    LikePutDirect,
    ShouldNotThrow,
    ShouldThrow,
}

/// The dense elements below the public length.
#[derive(Clone, Copy)]
pub enum Elements<'a> {
    /// `None` is a hole.
    Int32(&'a [Option<Value>]),
    /// NaN is a hole.
    Double(&'a [f64]),
    Contiguous(&'a [Option<Value>]),
}

/// The same, of an object that any of them can be stored into or removed from.
#[derive(Clone, Copy)]
pub enum ElementsMut<'a> {
    Int32(Int32Elements<'a>),
    Double(&'a [Cell<f64>]),
    Contiguous(Owned<'a, [Field<Option<Value>>]>),
}

#[derive(Clone, Copy)]
pub struct Int32Elements<'a>(&'a [Field<Option<Value>>]);

impl Int32Elements<'_> {
    pub fn len(self) -> usize {
        self.0.len()
    }

    pub fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    #[inline(always)]
    pub fn at(self, index: usize) -> Option<i32> {
        self.0[index].get().map(|value| value.to_bits() as i32)
    }

    #[inline(always)]
    pub fn set(self, index: usize, value: Option<i32>) {
        // SAFETY: no cell.
        unsafe { self.0[index].set_without_barrier(value.map(Value::int32)) }
    }
}

impl Elements<'_> {
    pub fn len(self) -> usize {
        match self {
            Elements::Int32(values) | Elements::Contiguous(values) => values.len(),
            Elements::Double(values) => values.len(),
        }
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    pub fn has_holes(self) -> bool {
        match self {
            Elements::Int32(values) | Elements::Contiguous(values) => values.contains(&None),
            Elements::Double(values) => values.iter().any(|value| value.is_nan()),
        }
    }
}

impl JsObject {
    #[inline(always)]
    pub fn element_kind(&self) -> ElementKind {
        self.indexing_mode().kind()
    }

    /// One more than the highest index that may be in use. Of an array, its length.
    #[inline(always)]
    pub fn public_length(&self) -> u32 {
        if self.indexing_mode().has_elements() {
            // SAFETY: it has lengths.
            unsafe { self.storage().public_length() }
        } else {
            0
        }
    }

    #[inline(always)]
    pub fn vector_length(&self) -> u32 {
        if self.indexing_mode().has_elements() {
            // SAFETY: as above.
            unsafe { self.storage().vector_length() }
        } else {
            0
        }
    }

    /// # Safety
    /// The object has elements.
    #[inline(always)]
    pub(super) unsafe fn element_words(&self) -> u32 {
        // SAFETY: the caller's.
        unsafe { self.storage().vector_length() }.max(1)
    }

    pub fn sparse_elements(&self) -> Option<Gc<SparseElements>> {
        // SAFETY: what the first slot has for that kind.
        (self.element_kind() == ElementKind::Sparse).then(|| unsafe {
            Gc::from_address(Value::bits_of(self.storage().value_at(0).get()) as usize)
        })
    }

    /// `None` unless the elements are dense. An object without any has an empty slice.
    #[inline(always)]
    pub fn elements<'a>(&'a self, _: &'a NoGc<'_>) -> Option<Elements<'a>> {
        // SAFETY: changing them takes the token exclusively, or the `Vm` that it keeps borrowed.
        unsafe { self.elements_unchecked() }
    }

    /// # Safety
    /// Nothing changes the elements or collects while the borrow lasts.
    #[inline(always)]
    pub unsafe fn elements_unchecked(&self) -> Option<Elements<'_>> {
        use core::slice::from_raw_parts;
        let (first, length) = (self.storage().elements_ptr(), self.public_length() as usize);
        // SAFETY: the caller's, and the kind says what they are.
        unsafe {
            Some(match self.element_kind() {
                ElementKind::None => Elements::Int32(&[]),
                ElementKind::Undecided | ElementKind::Int32 => {
                    Elements::Int32(from_raw_parts(first.cast(), length))
                }
                ElementKind::Double => Elements::Double(from_raw_parts(first.cast(), length)),
                ElementKind::Contiguous => {
                    Elements::Contiguous(from_raw_parts(first.cast(), length))
                }
                ElementKind::Sparse => return None,
            })
        }
    }

    #[inline(always)]
    pub fn elements_mut<'a>(&'a self, _: &'a mut NoGc<'_>) -> Option<ElementsMut<'a>> {
        let mode = self.indexing_mode().bits() & IndexingMode::KIND_AND_WRITABILITY_MASK;
        let storage = self.storage();
        // SAFETY: as in `elements`.
        unsafe {
            Some(if mode == ElementKind::Contiguous as u8 {
                let values = storage.values(storage.public_length());
                ElementsMut::Contiguous(Owned::new(self, values))
            } else if mode == ElementKind::Int32 as u8 {
                ElementsMut::Int32(Int32Elements(storage.values(storage.public_length())))
            } else if mode == ElementKind::Double as u8 {
                ElementsMut::Double(storage.doubles(storage.public_length()))
            } else {
                return None;
            })
        }
    }

    /// Dense, and neither shared nor restricted in any way.
    #[inline(always)]
    pub fn has_plain_writable_elements(&self) -> bool {
        let mode = self.indexing_mode();
        mode.kind().is_dense() && !mode.is_copy_on_write() && !mode.is_slow_put()
    }

    #[inline(always)]
    pub fn try_get_dense(&self, index: u32) -> Option<Value> {
        let storage = self.storage();
        // SAFETY: of that kind, and below the length.
        unsafe {
            match self.element_kind() {
                ElementKind::Int32 | ElementKind::Contiguous if index < storage.public_length() => {
                    storage.value_at(index).get()
                }
                ElementKind::Double if index < storage.public_length() => {
                    let number = storage.double_at(index).get();
                    (!number.is_nan()).then(|| Value::double(number))
                }
                _ => None,
            }
        }
    }

    /// If it takes neither more memory nor another kind.
    #[inline(always)]
    pub fn try_set_dense(&self, index: u32, value: Value) -> bool {
        let mode = self.indexing_mode().bits() & IndexingMode::KIND_AND_WRITABILITY_MASK;
        let storage = self.storage();
        // SAFETY: of that kind, and below the vector length.
        unsafe {
            if mode == ElementKind::Contiguous as u8 && index < storage.vector_length() {
                storage.value_at(index).set(self, Some(value));
            } else if mode == ElementKind::Int32 as u8
                && value.is_int32()
                && index < storage.vector_length()
            {
                storage.value_at(index).set_without_barrier(Some(value));
            } else if mode == ElementKind::Double as u8
                && index < storage.vector_length()
                && let Some(number) = value.as_number().filter(|number| !number.is_nan())
            {
                storage.double_at(index).set(number);
            } else {
                return false;
            }
            if index >= storage.public_length() {
                storage.set_public_length(index + 1);
            }
        }
        true
    }

    /// Whether reading a hole has to look further than this object.
    #[inline]
    pub fn holes_must_forward_to_prototype(&self) -> bool {
        let shape = self.shape();
        if let Some(global) = shape.global()
            && global.is_original_array_shape(&shape)
            && global.array_prototype_chain_is_sane()
        {
            return false;
        }
        shape.may_intercept_indexed_access()
            || self.chain_direct().skip(1).any(|prototype| {
                prototype.indexing_mode().has_elements()
                    || prototype.shape().may_intercept_indexed_access()
            })
    }

    fn dense_attributes(&self) -> Attributes {
        if !self.indexing_mode().is_slow_put() {
            return Attributes::NONE;
        }
        let (shape, mut attributes) = (self.shape(), Attributes::NONE);
        attributes.set(
            Attributes::DONT_DELETE,
            shape.has(ShapeFlags::ELEMENTS_ARE_SEALED),
        );
        attributes.set(
            Attributes::READ_ONLY,
            shape.has(ShapeFlags::ELEMENTS_ARE_FROZEN),
        );
        attributes
    }

    fn own_element(&self, index: u32) -> Option<SparseEntry> {
        match self.try_get_dense(index) {
            Some(value) => Some(SparseEntry {
                value,
                attributes: self.dense_attributes(),
            }),
            None => self.sparse_elements()?.get(index),
        }
    }

    pub(crate) fn count_dense_elements(&self) -> usize {
        // SAFETY: only reads.
        match unsafe { self.elements_unchecked() } {
            Some(Elements::Int32(values) | Elements::Contiguous(values)) => {
                values.iter().filter(|value| value.is_some()).count()
            }
            Some(Elements::Double(values)) => values.iter().filter(|it| !it.is_nan()).count(),
            None => 0,
        }
    }

    #[inline(always)]
    pub fn ensure_writable_elements(&self, vm: &Vm) {
        if self.indexing_mode().is_copy_on_write() {
            self.copy_shared_elements(vm)
                .unwrap_or_else(|_| bun_alloc::out_of_memory());
        }
    }

    #[inline(never)]
    fn copy_shared_elements(&self, vm: &Vm) -> Result<(), AllocError> {
        let (kind, length) = (self.element_kind(), self.public_length());
        let vector_length = Storage::optimal_vector_length(0, next_vector_length(length));
        let mut deferred = DeferredFire::new(vm);
        let transition = TransitionKind::allocate(kind);
        let shape = self.shape();
        let shape = shape.non_property_transition(vm, transition, &mut deferred);
        let storage = Storage::try_allocate(vm, 0, 0, Some(vector_length))?;
        // SAFETY: an object that shares its elements has no out-of-line slots.
        unsafe {
            self.storage().copy_words_to(storage, 0, length as usize);
            storage.set_lengths(length, vector_length);
            storage.clear_elements(kind, length, vector_length);
            self.set_shape_and_storage(&shape, storage, false);
        }
        Ok(())
    }

    /// Replaces the elements by `vector_length` holes of another kind, and has `fill` store into them.
    fn replace_elements(
        &self,
        vm: &Vm,
        kind: ElementKind,
        public_length: u32,
        vector_length: u32,
        fill: impl FnOnce(Storage),
    ) -> Result<(), AllocError> {
        if kind.is_dense()
            && !self.indexing_mode().is_slow_put()
            && (self.chain_may_intercept_indexed_access() || !self.shape().is_extensible())
        {
            self.non_property_transition(vm, TransitionKind::SwitchToSlowPut);
        }
        let mut deferred = DeferredFire::new(vm);
        let transition = TransitionKind::allocate(kind);
        let shape = self.shape();
        let shape = shape.non_property_transition(vm, transition, &mut deferred);
        let capacity = shape.out_of_line_capacity();
        let words = vector_length.max(1);
        let storage = Storage::try_allocate(vm, 0, capacity, Some(words))?;
        // SAFETY: new and of that size. The slots are copied, and the rest is cleared.
        unsafe {
            if capacity != 0 {
                self.storage()
                    .copy_words_to(storage, -1 - capacity as isize, capacity);
            }
            storage.set_lengths(public_length, vector_length);
            storage.clear_elements(kind, 0, words);
            fill(storage);
            self.set_shape_and_storage(&shape, storage, false);
        }
        Ok(())
    }

    pub(crate) fn convert_to_sparse(&self, vm: &Vm) -> Result<Gc<SparseElements>, AllocError> {
        if let Some(sparse) = self.sparse_elements() {
            return Ok(sparse);
        }
        let sparse = SparseElements::create(vm);
        let (length, attributes) = (self.public_length(), self.dense_attributes());
        for index in 0..length {
            if let Some(value) = self.try_get_dense(index) {
                sparse.insert(index, value, attributes);
            }
        }
        if attributes.contains(Attributes::READ_ONLY) {
            self.notify_presence_of_indexed_accessors(vm);
        }
        // SAFETY: there is one word, and the barrier comes with the shape.
        self.replace_elements(vm, ElementKind::Sparse, length, 0, |storage| unsafe {
            storage.value_at(0).set_without_barrier(Some(sparse.into()));
        })?;
        Ok(sparse)
    }

    fn convert_sparse_to_dense(
        &self,
        vm: &Vm,
        sparse: &SparseElements,
        length: u32,
    ) -> Result<(), AllocError> {
        let entries = sparse.entries();
        let capacity = self.shape().out_of_line_capacity();
        let vector_length = Storage::optimal_vector_length(capacity, length);
        let kind = entries
            .iter()
            .map(|entry| ElementKind::for_value(entry.1.value))
            .fold(ElementKind::Int32, ElementKind::max);
        self.replace_elements(vm, kind, length, vector_length, |storage| {
            for &(index, entry) in &entries {
                assert!(index < length);
                // SAFETY: inside, the kind can represent the value, and the barrier comes with the shape.
                unsafe { store_unchecked(storage, kind, index, entry.value) };
            }
        })
    }

    /// The elements are dense, writable and of a kind that is less general than `to`.
    #[inline(never)]
    fn generalize_elements(&self, vm: &Vm, to: ElementKind) {
        let from = self.element_kind();
        debug_assert!(from.is_dense() && from < to && to.is_dense());
        let mut deferred = DeferredFire::new(vm);
        let transition = TransitionKind::allocate(to);
        let shape = self.shape();
        let shape = shape.non_property_transition(vm, transition, &mut deferred);
        let storage = self.storage();
        // SAFETY: each word is read as the old kind and written as the new. A marker looks at neither.
        unsafe {
            let length = storage.vector_length();
            let words = core::slice::from_raw_parts_mut(storage.elements_ptr(), length as usize);
            if to == ElementKind::Double {
                for word in words {
                    *word = match *word {
                        0 => Value::PURE_NAN_BITS,
                        int32 => f64::from(int32 as i32).to_bits(),
                    };
                }
            } else if from == ElementKind::Double {
                for word in words {
                    let number = f64::from_bits(*word);
                    *word = Value::bits_of((!number.is_nan()).then(|| Value::double(number)));
                }
            }
            crate::heap::barrier::publish_fence();
            self.set_shape(&shape);
        }
    }

    /// Makes dense elements writable and able to represent what `kind` can.
    #[inline]
    pub fn ensure_element_kind(&self, vm: &Vm, kind: ElementKind) {
        assert!(self.element_kind().is_dense() && kind.is_dense());
        self.ensure_writable_elements(vm);
        if self.element_kind() < kind {
            self.generalize_elements(vm, kind);
        }
    }

    /// The elements are dense and writable.
    #[inline(never)]
    fn grow_vector(&self, vm: &Vm, length: u32) -> Result<(), AllocError> {
        if length > Storage::MAX_VECTOR_LENGTH {
            return Err(AllocError);
        }
        let capacity = self.shape().out_of_line_capacity();
        let vector_length = Storage::optimal_vector_length(capacity, next_vector_length(length));
        let storage = Storage::try_allocate(vm, 0, capacity, Some(vector_length))?;
        // SAFETY: larger than the old one in every part.
        unsafe {
            let old = self.storage();
            let used = old.public_length();
            let count = capacity + 1 + used as usize;
            old.copy_words_to(storage, -1 - capacity as isize, count);
            storage.set_lengths(used, vector_length);
            storage.clear_elements(self.element_kind(), used, vector_length);
            self.set_storage(storage);
        }
        Ok(())
    }

    /// Raises the public length. Panics unless the elements are dense.
    #[inline]
    pub(crate) fn ensure_public_length(&self, vm: &Vm, length: u32) -> Result<(), AllocError> {
        assert!(self.element_kind().is_dense());
        self.ensure_writable_elements(vm);
        if length > self.vector_length() {
            self.grow_vector(vm, length)?;
        }
        if length > self.public_length() {
            // SAFETY: inside the vector.
            unsafe { self.storage().set_public_length(length) };
        }
        Ok(())
    }

    /// Adds or replaces a plain data element, whatever the attributes or prototypes say.
    #[inline(never)]
    pub(crate) fn store_element(
        &self,
        vm: &Vm,
        index: u32,
        value: Value,
    ) -> Result<(), AllocError> {
        let needed = ElementKind::for_value(value);
        let vector_length = self.vector_length();
        let stays_dense = index < vector_length
            || (index < Storage::MAX_VECTOR_LENGTH
                && (index < MIN_SPARSE_DISTANCE
                    || (index / 2 <= vector_length
                        && (index < MIN_SPARSE_INDEX
                            || index == self.public_length()
                            || is_dense_enough(index, self.count_dense_elements())))));
        match self.element_kind() {
            ElementKind::None if stays_dense => {
                let capacity = self.shape().out_of_line_capacity();
                let vector_length = Storage::optimal_vector_length(capacity, index + 1);
                self.replace_elements(vm, needed, 0, vector_length, |_| {})?;
            }
            kind if kind.is_dense() && stays_dense => self.ensure_element_kind(vm, needed),
            _ => return self.store_sparse(vm, index, value, Attributes::NONE),
        }
        self.ensure_public_length(vm, index + 1)?;
        // SAFETY: inside, and the kind can represent the value.
        unsafe { store_unchecked(self.storage(), self.element_kind(), index, value) };
        self.write_barrier(value);
        Ok(())
    }

    fn store_sparse(
        &self,
        vm: &Vm,
        index: u32,
        value: Value,
        attributes: Attributes,
    ) -> Result<(), AllocError> {
        let sparse = self.convert_to_sparse(vm)?;
        if attributes.intersects(Attributes::READ_ONLY | Attributes::ACCESSOR) {
            self.notify_presence_of_indexed_accessors(vm);
        }
        let old_count = sparse.len();
        sparse.insert(index, value, attributes);
        let length = self.public_length().max(index + 1);
        // SAFETY: not dense.
        unsafe { self.storage().set_public_length(length) };
        if sparse.len() > old_count
            && sparse.has_only_plain_entries()
            && length <= Storage::MAX_VECTOR_LENGTH
            && is_dense_enough(length, sparse.len())
            && !self
                .shape()
                .has(ShapeFlags::ELEMENTS_ARE_SEALED | ShapeFlags::MAY_HAVE_INDEXED_ACCESSORS)
        {
            // It can stay sparse if there is no memory.
            let _ = self.convert_sparse_to_dense(vm, &sparse, length);
        }
        Ok(())
    }

    pub(crate) fn notify_presence_of_indexed_accessors(&self, vm: &Vm) {
        if let Some(global) = self.downcast::<GlobalObject>() {
            global
                .global_this()
                .notify_presence_of_indexed_accessors(vm);
        }
        if self.shape().may_intercept_indexed_access() {
            return;
        }
        self.non_property_transition(vm, TransitionKind::AddIndexedAccessors);
        if self.shape().may_be_prototype() {
            vm.did_add_indexed_interceptor_to_prototype();
        }
    }

    #[inline]
    pub fn ordinary_get_own_property_slot_by_index(
        &self,
        cx: &mut Cx,
        index: u32,
        slot: &mut PropertySlot,
    ) -> JsResult<bool> {
        if index == u32::MAX {
            return self.get_own_property_slot(cx, Identifier::from_index(index).name(), slot);
        }
        let Some(SparseEntry { value, attributes }) = self.own_element(index) else {
            return Ok(false);
        };
        if attributes.contains(Attributes::ACCESSOR) {
            slot.set_getter(self, attributes, &value.cast::<GetterSetter>());
        } else {
            slot.set_value(self, attributes, value);
        }
        Ok(true)
    }

    fn length_forbids(&self, index: u32) -> bool {
        self.indexing_mode().is_array()
            && index >= self.public_length()
            && self.shape().has(ShapeFlags::LENGTH_IS_READ_ONLY)
    }

    pub fn ordinary_put_by_index(
        &self,
        cx: &mut Cx,
        index: u32,
        value: Value,
        should_throw: bool,
    ) -> JsResult<bool> {
        if self.try_set_dense(index, value) {
            return Ok(true);
        }
        if index == u32::MAX {
            let name = Identifier::from_index(index);
            return self.set(cx, name.name(), value, should_throw);
        }
        let mode = self.indexing_mode();
        if mode.is_slow_put() || !mode.kind().is_dense() {
            if let Some(entry) = self.own_element(index) {
                if entry.attributes.contains(Attributes::ACCESSOR) {
                    let accessor = entry.value.cast::<GetterSetter>();
                    return accessor.call_setter(cx, self.as_value(), value, should_throw);
                }
                if !entry.attributes.is_writable() {
                    return cx.reject(should_throw, errors::READ_ONLY_PROPERTY_WRITE);
                }
                if let Some(sparse) = self.sparse_elements() {
                    sparse.insert(index, value, entry.attributes);
                    return Ok(true);
                }
            } else {
                if let Some(result) = self.intercept_put_on_hole(cx, index, value, should_throw)? {
                    return Ok(result);
                }
                if !self.shape().is_extensible() || self.length_forbids(index) {
                    return cx.reject(should_throw, errors::READ_ONLY_PROPERTY_WRITE);
                }
            }
        }
        self.store_element(cx, index, value)
            .map_err(|_| cx.throw_out_of_memory())?;
        Ok(true)
    }

    /// `Some` if a prototype has dealt with the store. No script has run otherwise.
    fn intercept_put_on_hole(
        &self,
        cx: &mut Cx,
        index: u32,
        value: Value,
        should_throw: bool,
    ) -> JsResult<Option<bool>> {
        if self.overrides_get_prototype() {
            return Ok(None);
        }
        let this = self.as_value();
        for prototype in self.chain_direct().skip(1) {
            if !prototype.shape().may_intercept_indexed_access()
                && !prototype.indexing_mode().has_elements()
            {
                continue;
            }
            if let Some(proxy) = prototype.downcast::<ProxyObject>() {
                return proxy
                    .put_index_with_receiver(cx, this, index, value, should_throw)
                    .map(Some);
            }
            if CellType::TYPED_ARRAYS.contains(prototype.cell_type()) {
                let length = prototype.cast::<JsArrayBufferView>().length();
                return Ok((index as usize >= length).then_some(true));
            }
            let mut slot = PropertySlot::new(this, crate::InternalMethod::Inquiry);
            if !prototype.get_own_property_slot_by_index(cx, index, &mut slot)? {
                continue;
            }
            if let Some(accessor) = slot.getter_setter() {
                return accessor
                    .call_setter(cx, this, value, should_throw)
                    .map(Some);
            }
            if !slot.attributes().is_writable() {
                return cx
                    .reject(should_throw, errors::READ_ONLY_PROPERTY_WRITE)
                    .map(Some);
            }
            return Ok(None);
        }
        Ok(None)
    }

    /// Defines an own data property with default attributes.
    #[inline(always)]
    pub fn put_direct_index(&self, cx: &mut Cx, index: u32, value: Value) -> JsResult<bool> {
        let mode = PutDirectIndexMode::LikePutDirect;
        self.put_direct_index_with(cx, index, value, Attributes::NONE, mode)
    }

    #[inline(always)]
    pub fn put_direct_index_with(
        &self,
        cx: &mut Cx,
        index: u32,
        value: Value,
        attributes: Attributes,
        mode: PutDirectIndexMode,
    ) -> JsResult<bool> {
        if attributes.is_empty() && self.try_set_dense(index, value) {
            return Ok(true);
        }
        self.put_direct_index_slow(cx, index, value, attributes, mode)
    }

    fn has_ordinary_elements(&self) -> bool {
        matches!(
            self.cell_type(),
            CellType::Array | CellType::DerivedArray | CellType::FinalObject
        ) || self.class().has_ordinary_define_own_property()
    }

    #[inline(never)]
    fn put_direct_index_slow(
        &self,
        cx: &mut Cx,
        index: u32,
        value: Value,
        attributes: Attributes,
        mode: PutDirectIndexMode,
    ) -> JsResult<bool> {
        let should_throw = mode == PutDirectIndexMode::ShouldThrow;
        if index == u32::MAX || !self.has_ordinary_elements() {
            let name = Identifier::from_index(index);
            let descriptor = PropertyDescriptor::stored(value, attributes);
            return self.define_own_property(cx, name.name(), &descriptor, should_throw);
        }
        let indexing_mode = self.indexing_mode();
        if attributes.is_empty()
            && indexing_mode.kind() != ElementKind::Sparse
            && !self
                .shape()
                .has(ShapeFlags::DID_PREVENT_EXTENSIONS | ShapeFlags::LENGTH_IS_READ_ONLY)
        {
            self.store_element(cx, index, value)
                .map_err(|_| cx.throw_out_of_memory())?;
            return Ok(true);
        }
        let descriptor = PropertyDescriptor::stored(value, attributes);
        let is_extensible =
            mode == PutDirectIndexMode::LikePutDirect || self.shape().is_extensible();
        self.define_element(cx, index, &descriptor, is_extensible, should_throw)
    }

    pub fn define_own_indexed_property(
        &self,
        cx: &mut Cx,
        index: u32,
        descriptor: &PropertyDescriptor,
        should_throw: bool,
    ) -> JsResult<bool> {
        let is_extensible = self.shape().is_extensible();
        self.define_element(cx, index, descriptor, is_extensible, should_throw)
    }

    fn define_element(
        &self,
        cx: &mut Cx,
        index: u32,
        descriptor: &PropertyDescriptor,
        is_extensible: bool,
        should_throw: bool,
    ) -> JsResult<bool> {
        if self.length_forbids(index) {
            return cx.reject(should_throw, errors::DEFINE_BEYOND_READ_ONLY_LENGTH);
        }
        let current = self.own_element(index).map(SparseEntry::descriptor);
        let current = current.as_ref();
        if !super::validate_property_descriptor(
            cx,
            is_extensible,
            descriptor,
            current,
            should_throw,
        )? {
            return Ok(false);
        }
        if let Some(current) = current
            && descriptor.changes_nothing(cx, current)?
        {
            return Ok(true);
        }
        let (value, attributes) = descriptor.apply_to(cx, current);
        let stored = if self.element_kind() != ElementKind::Sparse
            && attributes == self.dense_attributes()
        {
            self.store_element(cx, index, value)
        } else {
            self.store_sparse(cx, index, value, attributes)
        };
        stored.map_err(|_| cx.throw_out_of_memory())?;
        Ok(true)
    }

    pub fn ordinary_delete_property_by_index(&self, cx: &mut Cx, index: u32) -> JsResult<bool> {
        if index == u32::MAX {
            return self.delete(cx, Identifier::from_index(index).name());
        }
        let Some(entry) = self.own_element(index) else {
            return Ok(true);
        };
        if !entry.attributes.is_configurable() {
            return Ok(false);
        }
        if let Some(sparse) = self.sparse_elements() {
            sparse.remove(index);
        } else {
            self.ensure_writable_elements(cx);
            // SAFETY: there is an element at that index.
            unsafe {
                self.storage()
                    .clear_elements(self.element_kind(), index, index + 1);
            }
        }
        Ok(true)
    }

    pub fn own_indexed_property_names(&self, list: &mut PropertyNameList, mode: DontEnumMode) {
        if !self.indexing_mode().has_elements() || !list.includes_strings() {
            return;
        }
        match self.sparse_elements() {
            Some(sparse) => {
                let entries = sparse.sorted_entries();
                let included = entries.iter().filter(|it| mode.includes(it.1.attributes));
                list.add_indices(included.map(|entry| entry.0));
            }
            None => list.add_indices(
                (0..self.public_length()).filter(|&index| self.try_get_dense(index).is_some()),
            ),
        }
    }
}

pub(super) fn next_vector_length(length: u32) -> u32 {
    (length + (length >> 1) + 4).min(Storage::MAX_VECTOR_LENGTH)
}

/// # Safety
/// Below the vector length, `kind` is that of the storage and can represent the value, and the barrier follows.
#[inline(always)]
pub(super) unsafe fn store_unchecked(
    storage: Storage,
    kind: ElementKind,
    index: u32,
    value: Value,
) {
    // SAFETY: the caller's.
    unsafe {
        if kind == ElementKind::Double {
            let number = value.as_number().unwrap_or(f64::NAN);
            storage.double_at(index).set(number);
        } else {
            storage.value_at(index).set_without_barrier(Some(value));
        }
    }
}
