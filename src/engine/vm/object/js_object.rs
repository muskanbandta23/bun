use core::cell::UnsafeCell;
use core::mem::offset_of;
use core::ops::Deref;
use core::sync::atomic::{AtomicUsize, Ordering};

use bun_engine_macros::object_ops;
use bun_engine_text::PropertyName;

use crate::heap::{HeapAnalyzer, barrier, visit_children_of};
use crate::{
    Attributes, CallData, CellKind, CellType, CellTypeRange, Class, ClassOps, Cx, DeferredFire,
    DeletePropertySlot, DontEnumMode, Extends, Field, Gc, GcFree, GlobalObject, JsCell, JsResult,
    PropertyDescriptor, PropertyNameList, PropertyOffset, PropertySlot, PutPropertySlot, Shape,
    ShapeFlags, Storage, TransitionKind, TypeFlags, Value, Visitor, Vm, ascii_literal,
};

#[repr(C)]
pub struct JsObject {
    base: JsCell,
    storage: UnsafeCell<Storage>,
}

pub struct JsObjectInit {
    shape: Gc<Shape>,
    storage: Storage,
}

impl JsObjectInit {
    /// The properties that the shape has are undefined. Panics if the shape is a dictionary that is taken.
    #[inline(always)]
    pub fn new(vm: &Vm, shape: Gc<Shape>) -> JsObjectInit {
        if shape.max_offset().is_inline()
            && !shape.instance_indexing_mode().has_elements()
            && !shape.is_dictionary()
        {
            return JsObjectInit {
                shape,
                storage: Storage::NONE,
            };
        }
        Self::new_slow(vm, shape)
    }

    #[inline(never)]
    fn new_slow(vm: &Vm, shape: Gc<Shape>) -> JsObjectInit {
        if shape.is_dictionary() {
            assert!(!shape.has(ShapeFlags::IS_CLAIMED));
            shape.set_flags(ShapeFlags::IS_CLAIMED);
        }
        let (mode, capacity) = (shape.instance_indexing_mode(), shape.out_of_line_capacity());
        assert!(!mode.is_copy_on_write() && mode.kind() != crate::ElementKind::Sparse);
        let storage = if mode.has_elements() {
            let vector_length = Storage::optimal_vector_length(capacity, 0);
            Storage::try_create(vm, capacity, mode.kind(), 0, vector_length)
        } else if capacity != 0 {
            Storage::try_allocate(vm, 0, capacity, None)
        } else {
            Ok(Storage::NONE)
        };
        let storage = storage.unwrap_or_else(|_| bun_alloc::out_of_memory());
        if capacity != 0 {
            // SAFETY: new, with that many slots.
            unsafe {
                storage.clear_slots(0, capacity);
                for slot in storage.slots(shape.out_of_line_size()) {
                    slot.set_without_barrier(Some(Value::UNDEFINED));
                }
            }
        }
        JsObjectInit { shape, storage }
    }

    /// # Safety
    /// The storage is initialized, is what the shape describes, and refers to no cell that nothing else keeps alive
    /// until the object exists.
    #[inline(always)]
    pub unsafe fn with_storage(shape: Gc<Shape>, storage: Storage) -> JsObjectInit {
        JsObjectInit { shape, storage }
    }
}

#[unsafe(export_name = "engine_class_Object")]
static OBJECT_CLASS: Class =
    Class::new::<JsObject>(ascii_literal!("Object"), None, CellType::OBJECTS);

impl !GcFree for JsObject {}

impl Deref for JsObject {
    type Target = JsCell;

    #[inline(always)]
    fn deref(&self) -> &JsCell {
        &self.base
    }
}

// SAFETY: starts with a `JsCell`.
unsafe impl Extends<JsCell> for JsObject {}

// SAFETY: `visit_storage` visits what the storage has.
unsafe impl CellKind for JsObject {
    type Base = JsCell;
    type Init = JsObjectInit;

    const CLASS: &'static Class = &OBJECT_CLASS;
    const CELL_TYPE: CellType = CellType::Object;
    const FLAGS: TypeFlags = TypeFlags::empty();
    const HOLDS_CLASS: bool = false;
    const OPS: ClassOps = {
        let mut ops = JsObject::override_ops(ClassOps::CELL);
        ops.visit_children = visit_children_of::<JsObject>;
        ops.analyze_heap = analyze_heap;
        ops.estimated_size = estimated_size;
        ops
    };

    #[inline(always)]
    fn is(cell: &JsCell) -> bool {
        cell.is_object()
    }

    #[inline(always)]
    fn shape_of(init: &JsObjectInit) -> Gc<Shape> {
        init.shape
    }

    #[inline(always)]
    unsafe fn initialize(this: *mut JsObject, init: JsObjectInit) {
        // SAFETY: the caller's.
        unsafe {
            JsCell::initialize(this.cast(), init.shape);
            (&raw mut (*this).storage).write(UnsafeCell::new(init.storage));
        }
    }

    #[inline]
    fn trace_fields(&self, visitor: &mut Visitor) {
        self.visit_shape_and_storage(visitor);
    }
}

unsafe extern "C" fn analyze_heap(cell: &JsCell, analyzer: &mut HeapAnalyzer) {
    if let Some(object) = cell.as_object() {
        object.analyze_properties(analyzer);
    }
}

unsafe extern "C" fn estimated_size(cell: &JsCell, _: &Vm) -> usize {
    cell.cell_size() + cell.as_object().map_or(0, |object| object.storage_size())
}

#[object_ops]
impl JsObject {
    fn put(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        value: Value,
        slot: &mut PutPropertySlot,
    ) -> JsResult<bool> {
        self.ordinary_put(cx, name, value, slot)
    }

    fn put_by_index(
        &self,
        cx: &mut Cx,
        index: u32,
        value: Value,
        should_throw: bool,
    ) -> JsResult<bool> {
        self.ordinary_put_by_index(cx, index, value, should_throw)
    }

    fn delete_property(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        slot: &mut DeletePropertySlot,
    ) -> JsResult<bool> {
        self.ordinary_delete_property(cx, name, slot)
    }

    fn delete_property_by_index(&self, cx: &mut Cx, index: u32) -> JsResult<bool> {
        self.ordinary_delete_property_by_index(cx, index)
    }

    fn get_own_property_slot(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        slot: &mut PropertySlot,
    ) -> JsResult<bool> {
        self.ordinary_get_own_property_slot(cx, name, slot)
    }

    fn get_own_property_slot_by_index(
        &self,
        cx: &mut Cx,
        index: u32,
        slot: &mut PropertySlot,
    ) -> JsResult<bool> {
        self.ordinary_get_own_property_slot_by_index(cx, index, slot)
    }

    fn own_property_names(
        &self,
        cx: &mut Cx,
        list: &mut PropertyNameList,
        mode: DontEnumMode,
    ) -> JsResult<()> {
        self.ordinary_own_property_names(cx, list, mode)
    }

    fn define_own_property(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        descriptor: &PropertyDescriptor,
        should_throw: bool,
    ) -> JsResult<bool> {
        self.ordinary_define_own_property(cx, name, descriptor, should_throw)
    }

    fn prevent_extensions(&self, cx: &mut Cx) -> JsResult<bool> {
        self.ordinary_prevent_extensions(cx);
        Ok(true)
    }

    fn is_extensible(&self, _: &mut Cx) -> JsResult<bool> {
        Ok(self.shape().is_extensible())
    }

    fn set_prototype_of(
        &self,
        cx: &mut Cx,
        prototype: Value,
        should_throw: bool,
    ) -> JsResult<bool> {
        self.ordinary_set_prototype_of(cx, prototype, should_throw)
    }

    fn get_prototype_of(&self, _: &mut Cx) -> JsResult<Value> {
        Ok(self.prototype_direct())
    }
}

/// Calls the method of the class and reports the exception that it left.
macro_rules! dispatch {
    ($(fn $name:ident($($argument:ident: $type:ty),*) -> $result:ty;)*) => {$(
        #[inline]
        pub fn $name(&self, cx: &mut Cx, $($argument: $type),*) -> JsResult<$result> {
            // SAFETY: the method of the class of the object.
            let result = unsafe { (self.class().ops().$name)(self, cx, $($argument),*) };
            cx.check_exception()?;
            Ok(result)
        }
    )*};
}

impl JsObject {
    dispatch! {
        fn put_by_index(index: u32, value: Value, should_throw: bool) -> bool;
        fn delete_property(name: PropertyName<'_>, slot: &mut DeletePropertySlot) -> bool;
        fn delete_property_by_index(index: u32) -> bool;
        fn get_own_property_slot_by_index(index: u32, slot: &mut PropertySlot) -> bool;
        fn own_property_names(list: &mut PropertyNameList, mode: DontEnumMode) -> ();
        fn own_special_property_names(list: &mut PropertyNameList, mode: DontEnumMode) -> ();
        fn custom_has_instance(value: Value) -> bool;
        fn define_own_property(name: PropertyName<'_>, descriptor: &PropertyDescriptor, should_throw: bool) -> bool;
        fn prevent_extensions() -> bool;
        fn set_prototype_of(prototype: Value, should_throw: bool) -> bool;
    }

    #[inline(always)]
    pub fn get_own_property_slot(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        slot: &mut PropertySlot,
    ) -> JsResult<bool> {
        if !self.overrides_get_own_property_slot() {
            return self.ordinary_get_own_property_slot(cx, name, slot);
        }
        // SAFETY: the method of the class of the object.
        let found = unsafe { (self.class().ops().get_own_property_slot)(self, cx, name, slot) };
        cx.check_exception()?;
        Ok(found)
    }

    #[inline(always)]
    pub fn put(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        value: Value,
        slot: &mut PutPropertySlot,
    ) -> JsResult<bool> {
        let shape = self.shape();
        if !shape.type_flags().contains(TypeFlags::OVERRIDES_PUT) {
            return self.ordinary_put(cx, name, value, slot);
        }
        // SAFETY: as above.
        let result = unsafe { (shape.class().ops().put)(self, cx, name, value, slot) };
        cx.check_exception()?;
        Ok(result)
    }

    #[inline]
    pub fn is_extensible(&self, cx: &mut Cx) -> JsResult<bool> {
        let shape = self.shape();
        if !shape
            .type_flags()
            .contains(TypeFlags::OVERRIDES_IS_EXTENSIBLE)
        {
            return Ok(shape.is_extensible());
        }
        // SAFETY: as above.
        let result = unsafe { (shape.class().ops().is_extensible)(self, cx) };
        cx.check_exception()?;
        Ok(result)
    }

    /// An object or null.
    #[inline(always)]
    pub fn get_prototype_of(&self, cx: &mut Cx) -> JsResult<Value> {
        if !self.overrides_get_prototype() {
            return Ok(self.prototype_direct());
        }
        // SAFETY: as above.
        unsafe { (self.class().ops().get_prototype_of)(self, cx) }
    }

    pub(crate) fn call_data_of_class(&self) -> CallData {
        let mut data = CallData::NONE;
        // SAFETY: as above.
        unsafe { (self.class().ops().call_data)(self, &mut data) };
        data
    }

    pub(crate) fn construct_data_of_class(&self) -> CallData {
        let mut data = CallData::NONE;
        // SAFETY: as above.
        unsafe { (self.class().ops().construct_data)(self, &mut data) };
        data
    }
}

impl JsObject {
    pub const OFFSET_OF_STORAGE: usize = offset_of!(JsObject, storage);
    pub const MAX_PROTOTYPE_CHAIN_DEPTH: usize = 40_000;

    /// An object that has nothing native about it but its class, such as a prototype.
    pub fn create(vm: &Vm, shape: Gc<Shape>) -> Gc<JsObject> {
        vm.allocate::<JsObject>(JsObjectInit::new(vm, shape))
    }

    #[inline(always)]
    pub fn as_object(&self) -> Gc<JsObject> {
        self.into()
    }

    #[inline(always)]
    pub fn overrides_get_own_property_slot(&self) -> bool {
        self.inline_flags()
            .contains(TypeFlags::OVERRIDES_GET_OWN_PROPERTY_SLOT)
    }

    #[inline(always)]
    pub fn overrides_get_prototype(&self) -> bool {
        self.inline_flags()
            .contains(TypeFlags::OVERRIDES_GET_PROTOTYPE)
    }

    /// The realm that made the object.
    #[inline(always)]
    pub fn realm(&self) -> Option<Gc<GlobalObject>> {
        self.shape().global()
    }

    #[inline(always)]
    pub fn prototype_direct(&self) -> Value {
        self.shape().prototype()
    }

    #[inline(always)]
    pub fn prototype_object_direct(&self) -> Option<Gc<JsObject>> {
        self.shape().prototype_object()
    }

    /// The object and its prototypes, without asking exotic objects.
    pub fn chain_direct(&self) -> impl Iterator<Item = Gc<JsObject>> + use<> {
        core::iter::successors(Some(self.as_object()), |it| it.prototype_object_direct())
    }

    #[inline(always)]
    pub fn storage(&self) -> Storage {
        // SAFETY: only this thread writes it.
        unsafe { *self.storage.get() }
    }

    #[inline(always)]
    fn storage_atomic(&self) -> &AtomicUsize {
        // SAFETY: aligned. The plain loads are on the only thread that stores.
        unsafe { AtomicUsize::from_ptr(self.storage.get().cast()) }
    }

    #[inline(always)]
    pub(super) fn storage_concurrently(&self) -> Storage {
        Storage::from_address(self.storage_atomic().load(Ordering::Acquire))
    }

    /// # Safety
    /// The storage is initialized, is what the shape describes and has no pre-capacity.
    #[inline]
    pub unsafe fn set_storage(&self, storage: Storage) {
        if self.inline_flags().contains(TypeFlags::HAS_PRE_CAPACITY) {
            // SAFETY: the caller's.
            return unsafe { self.set_shape_and_storage(&self.shape(), storage, false) };
        }
        barrier::publish_fence();
        self.storage_atomic()
            .store(storage.address(), Ordering::Relaxed);
        self.write_barrier_for_cell();
    }

    /// `set_shape` has to follow before the next allocation.
    ///
    /// # Safety
    /// The storage is initialized and is what the next shape describes.
    #[inline]
    pub(crate) unsafe fn nuke_shape_and_set_storage(&self, storage: Storage, pre_capacity: bool) {
        if barrier::is_fenced() {
            self.nuke_shape_id();
            core::sync::atomic::fence(Ordering::Release);
        }
        if self.inline_flags().contains(TypeFlags::HAS_PRE_CAPACITY) != pre_capacity {
            self.set_own_flag(TypeFlags::HAS_PRE_CAPACITY, pre_capacity);
        }
        self.storage_atomic()
            .store(storage.address(), Ordering::Relaxed);
        barrier::publish_fence();
    }

    /// # Safety
    /// As for `set_shape`, with the new storage.
    #[inline]
    pub(crate) unsafe fn set_shape_and_storage(
        &self,
        shape: &Shape,
        storage: Storage,
        pre_capacity: bool,
    ) {
        // SAFETY: the caller's.
        unsafe {
            self.nuke_shape_and_set_storage(storage, pre_capacity);
            self.set_shape(shape);
        }
    }

    /// # Safety
    /// The cell or the storage has the slot.
    #[inline(always)]
    pub unsafe fn slot_unchecked(&self, offset: PropertyOffset) -> &Field<Option<Value>> {
        let base = if offset.is_inline() {
            self.address()
        } else {
            self.storage().address()
        };
        let address = base.wrapping_add_signed(offset.byte_offset());
        // SAFETY: the caller's.
        unsafe { &*core::ptr::with_exposed_provenance(address) }
    }

    /// # Safety
    /// As for `slot_unchecked`.
    #[inline(always)]
    pub unsafe fn get_direct_unchecked(&self, offset: PropertyOffset) -> Value {
        // SAFETY: the caller's.
        unsafe { self.slot_unchecked(offset) }
            .get()
            .unwrap_or_default()
    }

    /// # Safety
    /// As for `slot_unchecked`, and the value suits the attributes of the property.
    #[inline(always)]
    pub unsafe fn put_direct_unchecked(&self, offset: PropertyOffset, value: Value) {
        // SAFETY: the caller's.
        unsafe { self.slot_unchecked(offset).set(self, Some(value)) }
    }

    /// What the slot has, which is no value of the language for an accessor. Panics if there is no such slot.
    #[inline(always)]
    pub fn get_direct_at(&self, offset: PropertyOffset) -> Value {
        assert!(self.shape().is_valid_offset(offset));
        // SAFETY: checked.
        unsafe { self.get_direct_unchecked(offset) }
    }

    /// For the slot of a writable data property. Panics if there is no such slot or it has an accessor.
    #[inline(always)]
    pub fn put_direct_at(&self, vm: &Vm, offset: PropertyOffset, value: Value) {
        let shape = self.shape();
        assert!(shape.is_valid_offset(offset));
        // SAFETY: checked.
        unsafe {
            assert!(
                !shape.has(ShapeFlags::HAS_ANY_ACCESSOR)
                    || !self.get_direct_unchecked(offset).is_accessor_cell()
            );
            shape.did_replace_property(vm, offset);
            self.put_direct_unchecked(offset, value);
        }
    }

    /// What the slot of an own property in the shape has.
    #[inline]
    pub fn get_direct(&self, name: PropertyName<'_>) -> Option<Value> {
        self.get_direct_with_attributes(name).map(|found| found.0)
    }

    #[inline]
    pub fn get_direct_with_attributes(
        &self,
        name: PropertyName<'_>,
    ) -> Option<(Value, Attributes)> {
        let (offset, attributes) = self.shape().get(name)?;
        // SAFETY: from the shape.
        Some((unsafe { self.get_direct_unchecked(offset) }, attributes))
    }

    /// A storage with more slots, to go with a shape that has them. Ends the process if there is no memory.
    #[inline(never)]
    pub(crate) fn storage_with_more_slots(&self, vm: &Vm, old: usize, new: usize) -> Storage {
        debug_assert!(new > old);
        let has_elements = self.indexing_mode().has_elements();
        // SAFETY: each has the slots and elements that are copied.
        unsafe {
            let words = has_elements.then(|| self.element_words());
            let Ok(storage) = Storage::try_allocate(vm, 0, new, words) else {
                bun_alloc::out_of_memory()
            };
            storage.clear_slots(old, new);
            let count = old + words.map_or(0, |words| words as usize + 1);
            if count != 0 {
                self.storage()
                    .copy_words_to(storage, -1 - old as isize, count);
            }
            storage
        }
    }

    pub(crate) fn non_property_transition(&self, vm: &Vm, kind: TransitionKind) {
        let mut deferred = DeferredFire::new(vm);
        let shape = self
            .shape()
            .non_property_transition(vm, kind, &mut deferred);
        // SAFETY: the callers have converted the elements, where the kind concerns them.
        unsafe { self.set_shape(&shape) };
    }

    #[inline]
    pub fn did_become_prototype(&self, vm: &Vm) {
        if !self.shape().may_be_prototype() {
            self.become_prototype(vm);
        }
    }

    #[inline(never)]
    fn become_prototype(&self, vm: &Vm) {
        self.non_property_transition(vm, TransitionKind::BecomePrototype);
        if let Some(proxy) = self.downcast::<crate::GlobalProxy>() {
            proxy.target().did_become_prototype(vm);
        }
    }

    #[inline(always)]
    pub(crate) fn did_change_as_prototype(&self, vm: &Vm, shape: &Shape) {
        if shape.may_be_prototype() {
            vm.invalidate_prototype_chain_caches();
        }
    }

    pub fn chain_may_intercept_indexed_access(&self) -> bool {
        self.chain_direct()
            .any(|object| object.shape().may_intercept_indexed_access())
    }

    pub fn convert_to_dictionary(&self, vm: &Vm) {
        if !self.shape().is_dictionary() {
            self.convert_to(vm, ShapeFlags::CACHEABLE_DICTIONARY);
        }
    }

    pub fn convert_to_uncacheable_dictionary(&self, vm: &Vm) {
        if !self.shape().is_uncacheable_dictionary() {
            self.convert_to(vm, ShapeFlags::UNCACHEABLE_DICTIONARY);
        }
    }

    fn convert_to(&self, vm: &Vm, kind: ShapeFlags) {
        self.ensure_writable_elements(vm);
        let mut deferred = DeferredFire::new(vm);
        let shape = self.shape().to_dictionary(vm, kind, &mut deferred);
        // SAFETY: the same slots.
        unsafe { self.set_shape(&shape) };
        self.did_change_as_prototype(vm, &shape);
    }

    /// Makes the shape one that caches can rely on.
    pub fn flatten_dictionary(&self, vm: &Vm) {
        let old = self.shape();
        if !old.is_uncacheable_dictionary() {
            if old.is_dictionary() {
                old.flatten_cacheable_dictionary();
            }
            return;
        }
        let _defer = vm.heap.defer_collection();
        let mut deferred = DeferredFire::new(vm);
        let (new, old_offsets) = old.flatten_uncacheable_dictionary(vm, &mut deferred);
        let mut values = crate::heap::root_vec();
        values.extend(old_offsets.iter().map(|&offset| self.get_direct_at(offset)));
        let capacity = new.out_of_line_capacity();
        // SAFETY: the new storage has the capacity of the new shape, and every slot of it is written.
        unsafe {
            let words = self
                .indexing_mode()
                .has_elements()
                .then(|| self.element_words());
            let mut storage = Storage::NONE;
            if capacity != 0 || words.is_some() {
                storage = Storage::try_allocate(vm, 0, capacity, words)
                    .unwrap_or_else(|_| bun_alloc::out_of_memory());
                storage.clear_slots(0, capacity);
                if let Some(words) = words {
                    self.storage()
                        .copy_words_to(storage, -1, words as usize + 1);
                }
            }
            self.nuke_shape_and_set_storage(storage, false);
            for number in 0..new.inline_capacity().max(values.len()) {
                let offset = PropertyOffset::for_slot(number, new.inline_capacity());
                self.slot_unchecked(offset)
                    .set_without_barrier(values.get(number).copied());
            }
            self.set_shape(&new);
        }
    }

    pub fn is_callable(&self) -> bool {
        CellType::FUNCTIONS.contains(self.cell_type())
            || (self.inline_flags().contains(TypeFlags::OVERRIDES_CALL_DATA)
                && !self.call_data_of_class().is_none())
    }

    pub fn is_constructor(&self) -> bool {
        !CallData::for_construct(self.as_value()).is_none()
    }
}

/// The base of objects that own something outside the heap.
#[repr(C)]
pub struct JsDestructibleObject {
    base: JsObject,
    class: &'static Class,
}

impl JsDestructibleObject {
    pub const OFFSET_OF_CLASS: usize = offset_of!(JsDestructibleObject, class);
}

static DESTRUCTIBLE_OBJECT_CLASS: Class =
    Class::new::<JsDestructibleObject>(ascii_literal!("Object"), None, CellTypeRange::NONE);

impl !GcFree for JsDestructibleObject {}

impl Deref for JsDestructibleObject {
    type Target = JsObject;

    #[inline(always)]
    fn deref(&self) -> &JsObject {
        &self.base
    }
}

// SAFETY: starts with a `JsObject`.
unsafe impl<T> Extends<T> for JsDestructibleObject where JsObject: Extends<T> {}

// SAFETY: adds no reference to a cell.
unsafe impl CellKind for JsDestructibleObject {
    type Base = JsObject;
    type Init = JsObjectInit;

    const CLASS: &'static Class = &DESTRUCTIBLE_OBJECT_CLASS;
    const CELL_TYPE: CellType = CellType::Object;
    const FLAGS: TypeFlags = JsObject::FLAGS;
    const HOLDS_CLASS: bool = true;
    const OPS: ClassOps = JsObject::OPS;

    fn is(cell: &JsCell) -> bool {
        cell.inherits(Self::CLASS)
    }

    #[inline(always)]
    fn shape_of(init: &JsObjectInit) -> Gc<Shape> {
        init.shape
    }

    #[inline(always)]
    unsafe fn initialize(this: *mut JsDestructibleObject, init: JsObjectInit) {
        // SAFETY: the caller's.
        unsafe {
            (&raw mut (*this).class).write(init.shape.class());
            JsObject::initialize(this.cast(), init);
        }
    }

    #[inline]
    fn trace_fields(&self, visitor: &mut Visitor) {
        self.base.trace_fields(visitor);
    }
}
