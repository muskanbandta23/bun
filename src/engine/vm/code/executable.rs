use core::cell::{Cell, OnceCell, RefCell};
use core::mem::offset_of;
use std::rc::Rc;

use bun_engine_bytecode::{
    Code, CodeFlags, Eval, FunctionInfo, FunctionKind, FunctionRef, Module, Origin, Script,
    Specialization, Visibility,
};
use bun_engine_macros::{Trace, cell};
use bun_engine_text::{StringRef, Text, names};

use super::eval::EvalKey;
use crate::heap::{OnceBox, Owned};
use crate::{
    CellType, CodeBlock, Cx, Field, Gc, GlobalObject, InlineWatchpointSet, Intrinsic, JsArray,
    JsCell, JsFunction, JsResult, JsString, NativeFunction, RealmSlot, ScopeTemplate, Shape,
    SourceCode, SourceProviderRef, TracedMap, TypeFlags, Vm, WatchpointState, WeakField, call,
};

/// What a call goes through: it stores the code block in the new frame and calls the entry.
#[cell(flags = TypeFlags::SHAPE_IS_IMMORTAL)]
pub struct Executable {
    base: JsCell,
    entries: [Cell<usize>; 2],
    pub(self) call_code_block: Field<Option<Gc<CodeBlock>>>,
    pub(self) construct_code_block: Field<Option<Gc<CodeBlock>>>,
}

impl Executable {
    pub const fn offset_of_entry(specialization: Specialization) -> usize {
        offset_of!(Executable, entries) + specialization as usize * size_of::<usize>()
    }

    pub const fn offset_of_code_block(specialization: Specialization) -> usize {
        match specialization {
            Specialization::Call => offset_of!(Executable, call_code_block),
            Specialization::Construct => offset_of!(Executable, construct_code_block),
        }
    }

    pub(crate) fn init(shape: Gc<Shape>, call: usize, construct: usize) -> ExecutableInit {
        ExecutableInit {
            base: shape,
            entries: [Cell::new(call), Cell::new(construct)],
            call_code_block: None,
            construct_code_block: None,
        }
    }

    #[inline(always)]
    pub fn entry(&self, specialization: Specialization) -> usize {
        self.entries[specialization as usize].get()
    }

    #[inline(always)]
    pub fn code_block(&self, specialization: Specialization) -> Option<Gc<CodeBlock>> {
        match specialization {
            Specialization::Call => self.call_code_block(),
            Specialization::Construct => self.construct_code_block(),
        }
    }

    /// # Safety
    /// `entry` is code that expects to be called in that way, with that code block.
    pub unsafe fn install(
        &self,
        specialization: Specialization,
        entry: usize,
        code_block: Option<Gc<CodeBlock>>,
    ) {
        self.entries[specialization as usize].set(entry);
        match specialization {
            Specialization::Call => self.set_call_code_block(code_block),
            Specialization::Construct => self.set_construct_code_block(code_block),
        }
    }

    #[inline(always)]
    pub fn is_native(&self) -> bool {
        self.cell_type() == CellType::NativeExecutable
    }

    pub fn intrinsic(&self) -> Intrinsic {
        if let Some(native) = self.downcast::<NativeExecutable>() {
            return native.intrinsic;
        }
        self.downcast::<FunctionExecutable>()
            .map_or(Intrinsic::None, |function| function.intrinsic.get())
    }

    pub fn visibility(&self) -> Visibility {
        if let Some(native) = self.downcast::<NativeExecutable>() {
            return native.visibility;
        }
        self.downcast::<FunctionExecutable>()
            .map_or(Visibility::Public, |function| function.info.visibility())
    }
}

#[cell(cell_type = NativeExecutable, export = "engine_class_NativeExecutable")]
pub struct NativeExecutable {
    base: Executable,
    function: NativeFunction,
    constructor: NativeFunction,
    name: StringRef,
    pub(self) cached_source_text: Field<Option<Gc<JsString>>>,
    length: u32,
    intrinsic: Intrinsic,
    visibility: Visibility,
    is_named_as_bound: Cell<bool>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct NativeExecutableKey {
    function: usize,
    constructor: usize,
    name: StringRef,
    length: u32,
    visibility: u8,
}

// SAFETY: integers and a string.
unsafe impl crate::heap::PlainKey for NativeExecutableKey {}

impl NativeExecutable {
    pub const OFFSET_OF_FUNCTION: usize = offset_of!(NativeExecutable, function);
    pub const OFFSET_OF_CONSTRUCTOR: usize = offset_of!(NativeExecutable, constructor);

    pub fn create(
        vm: &Vm,
        length: u32,
        name: StringRef,
        function: NativeFunction,
        constructor: NativeFunction,
        intrinsic: Intrinsic,
        visibility: Visibility,
    ) -> Gc<NativeExecutable> {
        let call = (vm.hooks().native_call_entry)(vm, intrinsic);
        let shape = vm.shapes.native_executable();
        vm.allocate::<NativeExecutable>(NativeExecutableInit {
            base: Executable::init(shape, call, call::native_construct_entry()),
            function,
            constructor,
            name,
            cached_source_text: None,
            length,
            intrinsic,
            visibility,
            is_named_as_bound: Cell::new(false),
        })
    }

    /// One for all functions that are alike.
    pub fn shared(
        vm: &Vm,
        length: u32,
        name: StringRef,
        function: NativeFunction,
        constructor: NativeFunction,
        intrinsic: Intrinsic,
        visibility: Visibility,
    ) -> Gc<NativeExecutable> {
        let key = NativeExecutableKey {
            function: function as usize,
            constructor: constructor as usize,
            name: name.clone(),
            length,
            visibility: visibility as u8,
        };
        vm.native_executables.get(vm, &key).unwrap_or_else(|| {
            let executable = Self::create(
                vm,
                length,
                name,
                function,
                constructor,
                intrinsic,
                visibility,
            );
            vm.native_executables.insert(key, executable);
            executable
        })
    }

    #[inline(always)]
    pub fn function(&self) -> NativeFunction {
        self.function
    }

    #[inline(always)]
    pub fn constructor(&self) -> NativeFunction {
        self.constructor
    }

    pub fn name(&self) -> &StringRef {
        &self.name
    }

    pub fn length(&self) -> u32 {
        self.length
    }

    pub fn source_text(&self, cx: &Cx) -> JsResult<Gc<JsString>> {
        if let Some(text) = self.cached_source_text() {
            return Ok(text);
        }
        let text = native_source_text(cx, self.name.text())?;
        self.set_cached_source_text(Some(text));
        Ok(text)
    }

    /// Whether scripts see `bound ` in front of the name.
    pub fn is_named_as_bound(&self) -> &Cell<bool> {
        &self.is_named_as_bound
    }
}

#[cell(cell_type = ProgramExecutable..=FunctionExecutable)]
pub struct ScriptExecutable {
    base: Executable,
    provider: SourceProviderRef,
    top_level: Option<Gc<ScriptExecutable>>,
    rare_data: OnceBox<RareData>,
    never_optimize: Cell<bool>,
    never_fully_optimize: Cell<bool>,
    never_inline: Cell<bool>,
}

#[derive(Trace)]
struct RareData {
    template_objects: TracedMap<u32, Gc<JsArray>>,
    evals: TracedMap<EvalKey, Gc<EvalExecutable>>,
}

impl Default for RareData {
    fn default() -> RareData {
        RareData {
            template_objects: TracedMap::empty(),
            evals: TracedMap::empty(),
        }
    }
}

impl ScriptExecutable {
    fn init(
        shape: Gc<Shape>,
        provider: SourceProviderRef,
        top_level: Option<Gc<ScriptExecutable>>,
    ) -> ScriptExecutableInit {
        let (call, construct) = (call::link_call_entry(), call::link_construct_entry());
        ScriptExecutableInit {
            base: Executable::init(shape, call, construct),
            provider,
            top_level,
            rare_data: OnceBox::new(),
            never_optimize: Cell::new(false),
            never_fully_optimize: Cell::new(false),
            never_inline: Cell::new(false),
        }
    }

    pub fn provider(&self) -> &SourceProviderRef {
        &self.provider
    }

    /// The script, module or `eval` code that this is a part of.
    pub fn top_level(&self) -> Gc<ScriptExecutable> {
        self.top_level.unwrap_or_else(|| self.into())
    }

    /// Of the top level: by where the template ends.
    pub(crate) fn template_objects(&self) -> Owned<'_, TracedMap<u32, Gc<JsArray>>> {
        // SAFETY: traced with the rare data.
        unsafe { Owned::new(self, &self.rare_data.get_or_default().template_objects) }
    }

    /// What direct `eval` in this code has compiled.
    pub(super) fn evals(&self) -> Owned<'_, TracedMap<EvalKey, Gc<EvalExecutable>>> {
        // SAFETY: as above.
        unsafe { Owned::new(self, &self.rare_data.get_or_default().evals) }
    }

    /// A built-in whose source is not for scripts and tools to see.
    pub fn is_private_builtin(&self) -> bool {
        let function = self.downcast::<FunctionExecutable>();
        function.is_some_and(|it| it.info.origin().contains(Origin::NO_POSITIONS))
    }

    pub fn never_optimize(&self) -> &Cell<bool> {
        &self.never_optimize
    }

    pub fn never_fully_optimize(&self) -> &Cell<bool> {
        &self.never_fully_optimize
    }

    pub fn never_inline(&self) -> &Cell<bool> {
        &self.never_inline
    }

    /// The next call links the code again.
    pub fn clear_code(&self, specialization: Specialization) {
        let entry = match specialization {
            Specialization::Call => call::link_call_entry(),
            Specialization::Construct => call::link_construct_entry(),
        };
        // SAFETY: what it started with.
        unsafe { self.install(specialization, entry, None) }
    }

    pub(crate) fn unlinked_code(
        &self,
        cx: &Cx,
        specialization: Specialization,
    ) -> JsResult<Rc<Code>> {
        if let Some(function) = self.downcast::<FunctionExecutable>() {
            return function.compile(cx, specialization);
        }
        Ok(
            if let Some(program) = self.downcast::<ProgramExecutable>() {
                Rc::clone(&program.unit().code)
            } else if let Some(module) = self.downcast::<ModuleExecutable>() {
                Rc::clone(&module.unit().expect("code that may still run").code)
            } else {
                Rc::clone(&Gc::from(self).cast::<EvalExecutable>().unit().code)
            },
        )
    }
}

macro_rules! top_level_executables {
    ($($name:ident, $init:ident, $shape:ident, $unit:ty;)*) => {$(
        #[cell(cell_type = $name)]
        pub struct $name {
            base: ScriptExecutable,
            range: core::ops::Range<u32>,
            unit: RefCell<Option<Rc<$unit>>>,
        }

        impl $name {
            pub fn create(vm: &Vm, source: &SourceCode, unit: Rc<$unit>) -> Gc<$name> {
                vm.allocate::<$name>($init {
                    base: ScriptExecutable::init(vm.shapes.$shape(), source.provider.clone(), None),
                    range: source.range(),
                    unit: RefCell::new(Some(unit)),
                })
            }

            /// Panics once the code is released.
            pub fn unit(&self) -> Rc<$unit> {
                self.unit.borrow().clone().expect("code that may still run")
            }

            /// For code that does not run again. Its functions keep the executable alive.
            pub(crate) fn release_code(&self) {
                self.clear_code(Specialization::Call);
                self.unit.take();
            }

            pub fn range(&self) -> core::ops::Range<u32> {
                self.range.clone()
            }
        }
    )*};
}

top_level_executables! {
    ProgramExecutable, ProgramExecutableInit, program_executable, Script;
    EvalExecutable, EvalExecutableInit, eval_executable, Eval;
}

/// The records of a module in several loaders can have one together.
#[cell(cell_type = ModuleExecutable, export = "engine_class_ModuleExecutable")]
pub struct ModuleExecutable {
    base: ScriptExecutable,
    global: Gc<GlobalObject>,
    template: Gc<ScopeTemplate>,
    unit: RefCell<Option<Rc<Module>>>,
    functions: Rc<[FunctionInfo]>,
    flags: CodeFlags,
    /// By `Slot::function`.
    pub(self) lazy_functions: TracedMap<u32, Gc<FunctionExecutable>>,
    is_shareable: bool,
    is_shared: Cell<bool>,
    unfinished_records: Cell<u32>,
}

impl ModuleExecutable {
    pub fn create(
        cx: &Cx,
        source: &SourceCode,
        unit: Rc<Module>,
        is_shareable: bool,
    ) -> Gc<ModuleExecutable> {
        let shape = cx.shapes.module_executable();
        cx.allocate::<ModuleExecutable>(ModuleExecutableInit {
            base: ScriptExecutable::init(shape, source.provider.clone(), None),
            global: cx.global(),
            template: ScopeTemplate::create(cx, Rc::clone(&unit.scope)),
            functions: Rc::clone(&unit.code.functions),
            flags: unit.code.body.flags(),
            unit: RefCell::new(Some(unit)),
            is_shareable,
            is_shared: Cell::new(false),
            unfinished_records: Cell::new(1),
        })
    }

    pub fn global(&self) -> Gc<GlobalObject> {
        self.global
    }

    /// Of the environments that the code runs in.
    pub fn template(&self) -> Gc<ScopeTemplate> {
        self.template
    }

    pub fn flags(&self) -> CodeFlags {
        self.flags
    }

    /// `None` once the code is released.
    pub fn unit(&self) -> Option<Rc<Module>> {
        self.unit.borrow().clone()
    }

    /// If not, all code in the module runs in the one environment.
    pub fn is_shareable(&self) -> bool {
        self.is_shareable
    }

    /// For another record. False if `unit` is not what this was made from.
    pub(crate) fn share(&self, unit: &Rc<Module>) -> bool {
        if !self.is_shareable || !Rc::ptr_eq(&unit.scope, self.template.scope()) {
            return false;
        }
        self.unit.replace(Some(Rc::clone(unit)));
        self.is_shared.set(true);
        self.unfinished_records.update(|count| count + 1);
        true
    }

    /// One of the records does not run the code any more. What has been shared is likely to be shared again.
    pub(crate) fn did_finish_evaluation(&self) {
        self.unfinished_records.update(|count| count - 1);
        if self.unfinished_records.get() == 0 && !self.is_shared.get() {
            self.clear_code(Specialization::Call);
            self.unit.take();
        }
    }

    pub(crate) fn lazy_function(&self, vm: &Vm, index: u32) -> Gc<FunctionExecutable> {
        self.lazy_functions().get(&index).unwrap_or_else(|| {
            let info = FunctionRef::new(Rc::clone(&self.functions), index);
            let (provider, top_level) = (self.provider().clone(), Gc::from(self).upcast());
            let executable = FunctionExecutable::create(vm, provider, Some(top_level), info);
            if self.is_shareable {
                self.lazy_functions().insert(index, executable);
            }
            executable
        })
    }
}

#[cell(cell_type = FunctionExecutable, export = "engine_class_FunctionExecutable")]
pub struct FunctionExecutable {
    base: ScriptExecutable,
    info: FunctionRef,
    shape_slot: RealmSlot,
    /// Valid while at most one function has been made from this.
    singleton_watchpoints: InlineWatchpointSet,
    pub(self) first_function: WeakField<Option<Gc<JsFunction>>>,
    pub(self) cached_source_text: Field<Option<Gc<JsString>>>,
    name: OnceCell<StringRef>,
    intrinsic: Cell<Intrinsic>,
}

impl FunctionExecutable {
    pub const OFFSET_OF_SINGLETON_WATCHPOINTS: usize =
        offset_of!(FunctionExecutable, singleton_watchpoints);

    pub fn create(
        vm: &Vm,
        provider: SourceProviderRef,
        top_level: Option<Gc<ScriptExecutable>>,
        info: FunctionRef,
    ) -> Gc<FunctionExecutable> {
        vm.allocate::<FunctionExecutable>(FunctionExecutableInit {
            base: ScriptExecutable::init(vm.shapes.function_executable(), provider, top_level),
            shape_slot: Self::shape_slot_for(&info),
            info,
            singleton_watchpoints: InlineWatchpointSet::new(WatchpointState::Clear),
            first_function: None,
            cached_source_text: None,
            name: OnceCell::new(),
            intrinsic: Cell::new(Intrinsic::None),
        })
    }

    fn shape_slot_for(info: &FunctionInfo) -> RealmSlot {
        match (info.is_async(), info.is_generator()) {
            (true, true) => RealmSlot::async_generator_function_shape,
            (true, false) => RealmSlot::async_function_shape,
            (false, true) => RealmSlot::generator_function_shape,
            _ if info.origin().contains(Origin::BUILTIN) => RealmSlot::builtin_function_shape,
            _ if info.kind() == FunctionKind::Normal || info.kind().is_class_constructor() => {
                RealmSlot::function_shape
            }
            _ => RealmSlot::method_shape,
        }
    }

    /// Where a realm has the shape of the functions that are made from this.
    #[inline(always)]
    pub fn shape_slot(&self) -> RealmSlot {
        self.shape_slot
    }

    #[inline(always)]
    pub fn info(&self) -> &FunctionRef {
        &self.info
    }

    pub fn set_intrinsic(&self, intrinsic: Intrinsic) {
        self.intrinsic.set(intrinsic);
    }

    #[inline]
    pub fn is_builtin(&self) -> bool {
        self.info.origin().contains(Origin::BUILTIN)
    }

    pub fn has_prototype_property(&self) -> bool {
        self.info.is_generator()
            || (self.info.kind() == FunctionKind::Normal && !self.info.is_async())
            || self.info.kind().is_class_constructor()
    }

    pub fn name_text(&self) -> Text<'_> {
        self.info.name_text(self.provider().text())
    }

    /// What SetFunctionName gets, without the prefix. Allocates nothing in the heap.
    pub fn name(&self) -> &StringRef {
        self.name.get_or_init(|| {
            let name = self.name_text();
            if name == names::starDefaultText.text() && !self.info.has_own_name() {
                return names::default.to_identifier().into_string();
            }
            StringRef::from_text(name)
        })
    }

    fn is_default_constructor(&self) -> bool {
        self.info.origin().contains(Origin::DEFAULT_CONSTRUCTOR)
    }

    /// The text that the positions in the code refer to.
    pub fn code_provider(&self) -> SourceProviderRef {
        if self.is_default_constructor() {
            return self.heap().vm().default_constructor_provider(&self.info);
        }
        self.provider().clone()
    }

    /// In that text.
    pub fn parameters_start(&self) -> u32 {
        if self.is_default_constructor() {
            return "(function ".len() as u32;
        }
        self.info.parameters_start()
    }

    #[inline(always)]
    pub(crate) fn did_create(&self, vm: &Vm, function: Gc<JsFunction>) {
        if self.singleton_watchpoints.is_still_valid() {
            self.did_create_slow(vm, function);
        }
    }

    #[inline(never)]
    fn did_create_slow(&self, vm: &Vm, function: Gc<JsFunction>) {
        let is_first = self.singleton_watchpoints.state() == WatchpointState::Clear;
        self.set_first_function(is_first.then_some(function));
        self.singleton_watchpoints.touch(vm);
    }

    pub fn singleton(&self) -> Option<Gc<JsFunction>> {
        self.first_function()
    }

    #[inline(always)]
    pub fn singleton_watchpoints(&self) -> &InlineWatchpointSet {
        &self.singleton_watchpoints
    }

    pub fn source_text(&self, cx: &Cx) -> JsResult<Gc<JsString>> {
        if let Some(text) = self.cached_source_text() {
            return Ok(text);
        }
        let text = if self.is_private_builtin() && !self.info.kind().is_class_constructor() {
            native_source_text(cx, self.name_text())?
        } else {
            let range = self.info.text_range();
            let range = range.start as usize..range.end as usize;
            JsString::new(cx, StringRef::substring(self.provider().source(), range))
        };
        self.set_cached_source_text(Some(text));
        Ok(text)
    }
}

/// NativeFunction of ECMA-262.
pub fn native_source_text(cx: &Cx, name: Text<'_>) -> JsResult<Gc<JsString>> {
    let (before, after) = ("function ", "() { [native code] }");
    cx.new_string_from_parts(&[Text::from_ascii(before), name, Text::from_ascii(after)])
}
