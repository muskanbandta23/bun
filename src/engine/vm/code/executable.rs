use core::cell::Cell;
use core::mem::offset_of;
use std::rc::Rc;

use bun_engine_bytecode::{
    Code, Eval, FunctionKind, FunctionRef, Module, Origin, Script, Specialization, Visibility,
};
use bun_engine_macros::cell;
use bun_engine_text::{StringRef, Text};

use crate::{
    CellType, CodeBlock, Cx, Field, Gc, InlineWatchpointSet, Intrinsic, JsArray, JsCell,
    JsFunction, JsResult, JsString, NativeFunction, Shape, SourceCode, SourceProviderRef,
    TracedMap, TypeFlags, Vm, WatchpointState, WeakField, call,
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
    length: u32,
    intrinsic: Intrinsic,
    visibility: Visibility,
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
            length,
            intrinsic,
            visibility,
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
}

#[cell(cell_type = ProgramExecutable..=FunctionExecutable)]
pub struct ScriptExecutable {
    base: Executable,
    provider: SourceProviderRef,
    top_level: Option<Gc<ScriptExecutable>>,
    /// Of the top level: by where the template ends.
    pub(crate) template_objects: TracedMap<u32, Gc<JsArray>>,
    never_optimize: Cell<bool>,
    never_inline: Cell<bool>,
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
            never_optimize: Cell::new(false),
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

    pub fn never_optimize(&self) -> &Cell<bool> {
        &self.never_optimize
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
}

macro_rules! top_level_executables {
    ($($name:ident, $init:ident, $shape:ident, $unit:ty;)*) => {$(
        #[cell(cell_type = $name)]
        pub struct $name {
            base: ScriptExecutable,
            range: core::ops::Range<u32>,
            unit: Rc<$unit>,
        }

        impl $name {
            pub fn create(vm: &Vm, source: &SourceCode, unit: Rc<$unit>) -> Gc<$name> {
                vm.allocate::<$name>($init {
                    base: ScriptExecutable::init(vm.shapes.$shape(), source.provider.clone(), None),
                    range: source.range(),
                    unit,
                })
            }

            pub fn unit(&self) -> &Rc<$unit> {
                &self.unit
            }

            pub fn range(&self) -> core::ops::Range<u32> {
                self.range.clone()
            }
        }
    )*};
}

top_level_executables! {
    ProgramExecutable, ProgramExecutableInit, program_executable, Script;
    ModuleExecutable, ModuleExecutableInit, module_executable, Module;
    EvalExecutable, EvalExecutableInit, eval_executable, Eval;
}

#[cell(cell_type = FunctionExecutable, export = "engine_class_FunctionExecutable")]
pub struct FunctionExecutable {
    base: ScriptExecutable,
    info: FunctionRef,
    /// Valid while at most one function has been made from this.
    singleton_watchpoints: InlineWatchpointSet,
    pub(self) first_function: WeakField<Option<Gc<JsFunction>>>,
    pub(self) cached_source_text: Field<Option<Gc<JsString>>>,
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
            info,
            singleton_watchpoints: InlineWatchpointSet::new(WatchpointState::Clear),
            first_function: None,
            cached_source_text: None,
            intrinsic: Cell::new(Intrinsic::None),
        })
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

    pub fn singleton_watchpoints(&self) -> &InlineWatchpointSet {
        &self.singleton_watchpoints
    }

    pub fn source_text(&self, cx: &Cx) -> JsResult<Gc<JsString>> {
        if let Some(text) = self.cached_source_text() {
            return Ok(text);
        }
        let text = if self.is_builtin() && !self.info.kind().is_class_constructor() {
            native_source_text(cx, self.name_text())?
        } else {
            let range = self.info.text_range();
            let range = range.start as usize..range.end as usize;
            JsString::new(cx, StringRef::substring(self.provider().source(), range))
        };
        self.set_cached_source_text(Some(text));
        Ok(text)
    }

    pub(crate) fn unlinked_code(
        &self,
        cx: &Cx,
        specialization: Specialization,
    ) -> JsResult<Rc<Code>> {
        match self.info.code(specialization) {
            Some(code) => Ok(code),
            None => super::compile::compile_function(cx, self, specialization),
        }
    }
}

pub(crate) fn native_source_text(cx: &Cx, name: Text<'_>) -> JsResult<Gc<JsString>> {
    let (before, after) = ("function ", "() { [native code] }");
    cx.new_string_from_parts(&[Text::from_ascii(before), name, Text::from_ascii(after)])
}
