use core::cell::{Cell, RefCell};
use core::ffi::c_void;
use core::mem::offset_of;
use std::rc::Rc;

use bun_engine_macros::{cell, object_ops};
use bun_engine_number::Random;
use bun_engine_text::{PropertyName, StringRef, names};

use super::watchpoints::RealmWatchpoints;
use crate::runtime::control::RealmMicrotaskQueue;
use crate::{
    Attributes, Cx, Debugger, DeferredFire, DeletePropertySlot, DontEnumMode, ElementKind, Environment,
    Field, FieldArray, Gc, GcFree, GlobalBinding, GlobalBindings, GlobalProxy, IndexingMode,
    JsArray, JsCell, JsDestructibleObject, JsFinalObject, JsObject, JsObjectInit, JsResult,
    MicrotaskQueueRef, PropertyDescriptor, PropertyNameList, PropertySlot, PutPropertySlot,
    RealmHooks, RealmSlot, RealmWatchpoint, RegExpGlobalState, Shape, ShapeFlags, ShapeSpec,
    StaticProperty, TracedVec, TransitionKind, TypeFlags, Value, Vm, errors, static_table,
    validate_property_descriptor,
};

macro_rules! global_properties {
    ($($name:ident = $slot:ident,)*) => {
        static_table!(static GLOBAL_PROPERTIES = [
            $(StaticProperty::realm_slot(names::$name, RealmSlot::$slot),)*
        ]);
    };
}

global_properties! {
    isNaN = is_nan_function,
    isFinite = is_finite_function,
    escape = escape_function,
    unescape = unescape_function,
    decodeURI = decode_uri_function,
    decodeURIComponent = decode_uri_component_function,
    encodeURI = encode_uri_function,
    encodeURIComponent = encode_uri_component_function,
    eval = eval_function,
    globalThis = global_this,
    parseInt = parse_int_function,
    parseFloat = parse_float_function,
    ArrayBuffer = array_buffer_constructor,
    EvalError = eval_error_constructor,
    RangeError = range_error_constructor,
    ReferenceError = reference_error_constructor,
    SyntaxError = syntax_error_constructor,
    TypeError = type_error_constructor,
    URIError = uri_error_constructor,
    AggregateError = aggregate_error_constructor,
    SuppressedError = suppressed_error_constructor,
    Proxy = proxy_constructor,
    Reflect = reflect_object,
    JSON = json_object,
    Math = math_object,
    Atomics = atomics_object,
    WebAssembly = webassembly_object,
    console = console_object,
    Int8Array = int8_array_constructor,
    Int16Array = int16_array_constructor,
    Int32Array = int32_array_constructor,
    Uint8Array = uint8_array_constructor,
    Uint8ClampedArray = uint8_clamped_array_constructor,
    Uint16Array = uint16_array_constructor,
    Uint32Array = uint32_array_constructor,
    Float16Array = float16_array_constructor,
    Float32Array = float32_array_constructor,
    Float64Array = float64_array_constructor,
    BigInt64Array = bigint64_array_constructor,
    BigUint64Array = biguint64_array_constructor,
    DataView = data_view_constructor,
    Date = date_constructor,
    Error = error_constructor,
    Boolean = boolean_constructor,
    Map = map_constructor,
    Number = number_constructor,
    Set = set_constructor,
    WeakMap = weak_map_constructor,
    WeakSet = weak_set_constructor,
    WeakRef = weak_ref_constructor,
    FinalizationRegistry = finalization_registry_constructor,
    Object = object_constructor,
    Function = function_constructor,
    Array = array_constructor,
    RegExp = regexp_constructor,
    Iterator = iterator_constructor,
    SharedArrayBuffer = shared_array_buffer_constructor,
    DisposableStack = disposable_stack_constructor,
    AsyncDisposableStack = async_disposable_stack_constructor,
    String = string_constructor,
    Promise = promise_constructor,
    BigInt = bigint_constructor,
    Symbol = symbol_constructor,
    Intl = intl_object,
    Temporal = temporal_object,
    ShadowRealm = shadow_realm_constructor,
}

/// Indexed by the bits of the kind and of copy on write.
const ARRAY_SHAPE_COUNT: usize = 16;

fn array_shape_index(mode: IndexingMode) -> usize {
    usize::from(mode.bits() >> 1) % ARRAY_SHAPE_COUNT
}

#[cell(name = "GlobalObject", cell_type = GlobalObject, ops, static_table = GLOBAL_PROPERTIES,
    export = "engine_class_GlobalObject", flags = TypeFlags::IS_IMMUTABLE_PROTOTYPE_EXOTIC_OBJECT)]
pub struct GlobalObject {
    base: JsDestructibleObject,
    vm: *const Vm,
    hooks: &'static RealmHooks,
    embedder_methods: *const c_void,
    overridden_date_now: Cell<f64>,
    console_client: Cell<*mut c_void>,
    /// A `with` scope that is looked in after the global object.
    pub scope_extension: Field<Option<Gc<Environment>>>,
    pub(self) original_array_shapes: FieldArray<Option<Gc<Shape>>, ARRAY_SHAPE_COUNT>,
    pub(self) array_shapes_for_allocation: FieldArray<Option<Gc<Shape>>, ARRAY_SHAPE_COUNT>,
    /// The last field whose offset C++ knows.
    pub(super) slots: FieldArray<Option<Gc<JsCell>>, { RealmSlot::COUNT }>,
    watchpoints: RealmWatchpoints,
    variables: GlobalBindings,
    lexicals: GlobalBindings,
    /// Changes when a lexical binding is declared, which may hide a property.
    lexical_epoch: Cell<u32>,
    is_initialized: Cell<bool>,
    needs_debug_hooks: Cell<bool>,
    discards_microtasks: Cell<bool>,
    /// What other crates keep for the realm. See `extension_cell`.
    pub(self) extension_cells: TracedVec<Option<Gc<JsCell>>>,
    microtask_queue: RealmMicrotaskQueue,
    regexp_state: RegExpGlobalState,
    random: Cell<Random>,
    eval_disabled_message: RefCell<Option<StringRef>>,
    webassembly_disabled_message: RefCell<Option<StringRef>>,
    debugger: DebuggerSlot,
}

#[derive(Default)]
struct DebuggerSlot(RefCell<Option<Rc<dyn Debugger>>>);

// SAFETY: a supertrait.
unsafe impl GcFree for DebuggerSlot {}

#[object_ops(export = "engine_GlobalObject")]
impl GlobalObject {
    fn get_own_property_slot(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        slot: &mut PropertySlot,
    ) -> JsResult<bool> {
        self.get_declared_property_slot(cx, name, slot)
    }

    fn put(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        value: Value,
        slot: &mut PutPropertySlot,
    ) -> JsResult<bool> {
        let Some(binding) = self.variables.find(name) else {
            return self.ordinary_put(cx, name, value, slot);
        };
        if !binding.attributes().is_writable() {
            return cx.reject(slot.is_strict(), errors::READ_ONLY_PROPERTY_WRITE);
        }
        if !slot.is_receiver(self) {
            return JsObject::define_property_on_receiver(cx, name, value, slot);
        }
        // SAFETY: one of its own.
        unsafe { binding.set(cx, self, value) };
        Ok(true)
    }

    fn delete_property(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        slot: &mut DeletePropertySlot,
    ) -> JsResult<bool> {
        if self.variables.find(name).is_some() {
            return Ok(false);
        }
        self.ordinary_delete_property(cx, name, slot)
    }

    fn own_special_property_names(
        &self,
        _: &mut Cx,
        list: &mut PropertyNameList,
        mode: DontEnumMode,
    ) -> JsResult<()> {
        for name in self.variables.names() {
            let binding = self.variables.find(name.name());
            if binding.is_some_and(|binding| mode.includes(binding.attributes())) {
                list.add(name.name());
            }
        }
        Ok(())
    }

    fn define_own_property(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        descriptor: &PropertyDescriptor,
        should_throw: bool,
    ) -> JsResult<bool> {
        let Some(binding) = self.variables.find(name) else {
            return self.ordinary_define_own_property(cx, name, descriptor, should_throw);
        };
        let attributes = binding.attributes() | Attributes::DONT_DELETE;
        let current = PropertyDescriptor::data(binding.get().unwrap_or_default(), attributes);
        if !validate_property_descriptor(cx, false, descriptor, Some(&current), should_throw)? {
            return Ok(false);
        }
        if let Some(value) = descriptor.value() {
            // SAFETY: one of its own.
            unsafe { binding.set(cx, self, value) };
        }
        if descriptor.has_writable() && !descriptor.writable() && current.writable() {
            binding.make_read_only(cx);
            self.watchpoints_unchecked(RealmWatchpoint::GlobalVariablesAreWritable)
                .invalidate(cx);
        }
        Ok(true)
    }
}

impl GlobalObject {
    pub const OFFSET_OF_VM: usize = offset_of!(GlobalObject, vm);
    pub const OFFSET_OF_LEXICAL_EPOCH: usize = offset_of!(GlobalObject, lexical_epoch);
    pub const OFFSET_OF_MICROTASK_QUEUE: usize = offset_of!(GlobalObject, microtask_queue);
    pub const OFFSET_OF_RANDOM: usize = offset_of!(GlobalObject, random);
    pub const OFFSET_OF_REGEXP_STATE: usize = offset_of!(GlobalObject, regexp_state);

    pub const fn offset_of_slot(slot: RealmSlot) -> usize {
        offset_of!(GlobalObject, slots) + slot as usize * size_of::<u64>()
    }

    pub const fn offset_of_watchpoints(which: RealmWatchpoint) -> usize {
        offset_of!(GlobalObject, watchpoints)
            + RealmWatchpoints::OFFSET_OF_SETS
            + which as usize * size_of::<usize>()
    }

    /// For a type that extends this one. `finish_creation` follows.
    pub fn init(
        vm: &Vm,
        shape: Gc<Shape>,
        hooks: &'static RealmHooks,
        embedder_methods: *const c_void,
    ) -> GlobalObjectInit {
        GlobalObjectInit {
            base: JsObjectInit::new(vm, shape),
            vm,
            hooks,
            embedder_methods,
            original_array_shapes: [None; ARRAY_SHAPE_COUNT],
            array_shapes_for_allocation: [None; ARRAY_SHAPE_COUNT],
            slots: [None; RealmSlot::COUNT],
            watchpoints: RealmWatchpoints::new(),
            variables: GlobalBindings::default(),
            lexicals: GlobalBindings::default(),
            lexical_epoch: Cell::new(1),
            is_initialized: Cell::new(false),
            needs_debug_hooks: Cell::new(false),
            discards_microtasks: Cell::new(false),
            scope_extension: None,
            microtask_queue: RealmMicrotaskQueue::new(vm.default_microtask_queue().clone()),
            regexp_state: RegExpGlobalState::default(),
            random: Cell::new(Random::new(vm.random_seed() as u32)),
            overridden_date_now: Cell::new(f64::NAN),
            eval_disabled_message: RefCell::new(None),
            webassembly_disabled_message: RefCell::new(None),
            console_client: Cell::new(core::ptr::null_mut()),
            debugger: DebuggerSlot::default(),
        }
    }

    pub fn create(vm: &Vm, hooks: &'static RealmHooks) -> Gc<GlobalObject> {
        Self::create_with(vm, hooks, ShapeSpec::of::<GlobalObject>())
    }

    /// Scripts can change its prototype.
    pub fn create_for_shadow_realm(vm: &Vm, hooks: &'static RealmHooks) -> Gc<GlobalObject> {
        let spec = ShapeSpec::of::<GlobalObject>().with_mutable_prototype();
        Self::create_with(vm, hooks, spec)
    }

    fn create_with(vm: &Vm, hooks: &'static RealmHooks, spec: ShapeSpec) -> Gc<GlobalObject> {
        let shape = Shape::create(vm, None, Value::NULL, spec);
        let init = Self::init(vm, shape, hooks, core::ptr::null());
        let global = vm.allocate::<GlobalObject>(init);
        global.finish_creation(vm);
        global
    }

    pub fn finish_creation(&self, vm: &Vm) {
        let _defer = vm.heap.defer_collection();
        self.shape().set_global(Some(self.into()));
        // SAFETY: as in `initialize_slot`.
        let cx: &Cx = unsafe { Cx::from_global(self.into()) };
        (vm.hooks().initialize_realm)(cx);
        for (name, value) in [
            (names::undefined, Value::UNDEFINED),
            (names::NaN, Value::NAN),
            (names::Infinity, Value::double(f64::INFINITY)),
        ] {
            self.add_static_global(vm, name, value, Attributes::CONSTANT);
        }
        let last = self.chain_direct().last().unwrap_or(self.as_object());
        if last != self.object_prototype() {
            last.set_prototype_direct(vm, self.object_prototype().into());
        }
        self.init_global_this(GlobalProxy::create(vm, self.into()));
        for only_instance in [
            self.as_object(),
            self.object_prototype(),
            self.function_prototype(),
            self.array_prototype(),
            self.string_prototype(),
            self.iterator_prototype(),
            self.array_iterator_prototype(),
        ] {
            only_instance.shape().set_flags(ShapeFlags::IS_CLAIMED);
        }
        self.is_initialized.set(true);
        self.watchpoints(vm, RealmWatchpoint::ArrayPrototypeChainIsSane);
        self.watchpoints(vm, RealmWatchpoint::ArrayIteratorProtocol);
    }

    /// Whatever a class that extends this one makes of it.
    pub(super) fn get_declared_property_slot(
        &self,
        cx: &mut Cx,
        name: PropertyName<'_>,
        slot: &mut PropertySlot,
    ) -> JsResult<bool> {
        if self.ordinary_get_own_property_slot(cx, name, slot)? {
            return Ok(true);
        }
        let Some(binding) = self.variables.find(name) else {
            return Ok(false);
        };
        let attributes = binding.attributes() | Attributes::DONT_DELETE;
        slot.set_value(self, attributes, binding.get().unwrap_or_default());
        Ok(true)
    }

    /// Whether a debugger wants to step through the code that is compiled from now on.
    pub fn needs_debug_hooks(&self) -> &Cell<bool> {
        &self.needs_debug_hooks
    }

    pub(super) fn is_initialized(&self) -> bool {
        self.is_initialized.get()
    }

    #[inline(always)]
    pub(crate) fn vm_ptr(&self) -> *const Vm {
        self.vm
    }

    #[inline(always)]
    pub fn hooks(&self) -> &'static RealmHooks {
        self.hooks
    }

    #[inline(always)]
    pub(super) fn realm_watchpoints(&self) -> &RealmWatchpoints {
        &self.watchpoints
    }

    pub fn variables(&self) -> &GlobalBindings {
        &self.variables
    }

    pub fn lexicals(&self) -> &GlobalBindings {
        &self.lexicals
    }

    #[inline(always)]
    pub fn lexical_epoch(&self) -> u32 {
        self.lexical_epoch.get()
    }

    pub(crate) fn add_variable(
        &self,
        vm: &Vm,
        name: PropertyName<'_>,
        attributes: Attributes,
    ) -> &GlobalBinding {
        vm.invalidate_prototype_chain_caches();
        self.variables.add(self, name, attributes, true)
    }

    pub(crate) fn add_lexical(&self, vm: &Vm, name: PropertyName<'_>, attributes: Attributes) {
        self.lexicals.add(self, name, attributes, false);
        self.lexical_epoch.update(|epoch| epoch + 1);
        if self.shape().get(name).is_some() {
            self.change_shape_id(vm);
        }
        (vm.hooks().did_declare_global_lexical)(vm, self, name);
    }

    pub fn add_static_global(
        &self,
        vm: &Vm,
        name: PropertyName<'_>,
        value: Value,
        attributes: Attributes,
    ) {
        let binding = self.add_variable(vm, name, attributes);
        // SAFETY: one of its own.
        unsafe { binding.set(vm, self, value) };
    }

    /// Once `Array.prototype` exists.
    pub fn create_array_shapes(&self, vm: &Vm) {
        use ElementKind::{Contiguous, Double, Int32, Sparse, Undecided};
        let plain = [Undecided, Int32, Double, Contiguous, Sparse].map(IndexingMode::array);
        let shared = [Int32, Double, Contiguous].map(|it| IndexingMode::array(it).copy_on_write());
        for mode in plain.into_iter().chain(shared) {
            let prototype = self.array_prototype().into();
            let shape = Some(JsArray::create_shape(vm, self.into(), prototype, mode));
            let index = array_shape_index(mode);
            self.original_array_shapes().set(index, shape);
            self.array_shapes_for_allocation().set(index, shape);
        }
    }

    pub(super) fn switch_array_shapes_to_slow_put(&self, vm: &Vm) {
        let (shapes, kind) = (
            self.array_shapes_for_allocation(),
            TransitionKind::SwitchToSlowPut,
        );
        for index in 0..ARRAY_SHAPE_COUNT {
            if let Some(shape) = shapes.at(index) {
                let shape = shape.non_property_transition(vm, kind, &mut DeferredFire::new(vm));
                shapes.set(index, Some(shape));
            }
        }
        for slot in [
            RealmSlot::cloned_arguments_shape,
            RealmSlot::regexp_match_array_shape,
            RealmSlot::regexp_match_array_with_indices_shape,
            RealmSlot::regexp_indices_array_shape,
        ] {
            if let Some(shape) = self.slot_if_initialized(slot) {
                let shape = shape.cast::<Shape>();
                let shape = shape.non_property_transition(vm, kind, &mut DeferredFire::new(vm));
                self.slots().set(slot as usize, Some(shape.cell()));
            }
        }
        (vm.hooks().did_start_having_a_bad_time)(vm, self);
    }

    #[inline(always)]
    pub fn original_array_shape(&self, mode: IndexingMode) -> Gc<Shape> {
        self.original_array_shapes()
            .at(array_shape_index(mode))
            .expect("a mode that arrays have")
    }

    #[inline(always)]
    pub fn is_original_array_shape(&self, shape: &Shape) -> bool {
        let index = array_shape_index(shape.instance_indexing_mode());
        self.original_array_shapes().at(index) == Some(shape.into())
    }

    /// What to make a new array with.
    #[inline(always)]
    pub fn array_shape(&self, kind: ElementKind) -> Gc<Shape> {
        self.array_shapes_for_allocation()
            .at(array_shape_index(IndexingMode::array(kind)))
            .expect("a kind that arrays have")
    }

    pub fn new_object_with_prototype(&self, vm: &Vm, prototype: Gc<JsObject>) -> Gc<JsObject> {
        let shape = if prototype == self.object_prototype() {
            self.object_shape()
        } else {
            let capacity = JsFinalObject::DEFAULT_INLINE_CAPACITY;
            Shape::for_empty_object(vm, self.into(), prototype, capacity)
        };
        JsFinalObject::create(vm, shape).upcast()
    }

    /// A crate that needs more than the slots picks an index of its own.
    pub fn extension_cell(&self, index: usize) -> Option<Gc<JsCell>> {
        self.extension_cells().get(index).flatten()
    }

    pub fn set_extension_cell(&self, index: usize, cell: Gc<JsCell>) {
        let cells = self.extension_cells();
        cells.resize(cells.len().max(index + 1), None);
        cells.set(index, Some(cell));
    }

    pub fn microtask_queue(&self) -> MicrotaskQueueRef {
        self.microtask_queue.get()
    }

    pub fn set_microtask_queue(&self, queue: MicrotaskQueueRef) {
        self.microtask_queue.set(queue);
    }

    #[inline(always)]
    pub(crate) fn realm_microtask_queue(&self) -> &RealmMicrotaskQueue {
        &self.microtask_queue
    }

    /// Whether the microtasks of the realm are dropped when it is their turn.
    #[inline(always)]
    pub fn discards_microtasks(&self) -> &Cell<bool> {
        &self.discards_microtasks
    }

    #[inline(always)]
    pub(crate) fn regexp_state(&self) -> &RegExpGlobalState {
        &self.regexp_state
    }

    /// That of `Math.random`.
    pub fn random_state(&self) -> &Cell<Random> {
        &self.random
    }

    /// NaN if it is not.
    pub fn overridden_date_now(&self) -> &Cell<f64> {
        &self.overridden_date_now
    }

    pub fn eval_disabled_message(&self) -> &RefCell<Option<StringRef>> {
        &self.eval_disabled_message
    }

    pub fn webassembly_disabled_message(&self) -> &RefCell<Option<StringRef>> {
        &self.webassembly_disabled_message
    }

    pub fn console_client(&self) -> &Cell<*mut c_void> {
        &self.console_client
    }

    #[inline(always)]
    pub(crate) fn debugger_slot(&self) -> &RefCell<Option<Rc<dyn Debugger>>> {
        &self.debugger.0
    }
}
