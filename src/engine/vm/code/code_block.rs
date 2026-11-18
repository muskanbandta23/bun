use core::alloc::Layout;
use core::any::Any;
use core::cell::{Cell, RefCell};
use core::mem::offset_of;
use core::ptr::NonNull;
use std::rc::Rc;

use bun_engine_bytecode::{
    ArrayLiteral, Body, Code, CodeFlags, CodeKind, Constant, FunctionIndex, FunctionRef,
    IdentifierIndex, Instructions, Opcode, Position, Register, ScopeIndex, Specialization,
    SuspendPoint, SuspendPointIndex, TemplateSite,
};
use bun_engine_macros::cell;
use bun_engine_text::{Identifier, PropertyName, names};

use super::metadata;
use super::optimization::Optimization;
use super::profile::ValueProfile;
use crate::heap::Survivors;
use crate::{
    Attributes, Cx, Field, FunctionExecutable, Gc, GcFree, GlobalObject, ImmutableArray,
    IntegrityLevel, JsArray, JsBigInt, JsCell, JsResult, JsString, RealmSlot, RegExp,
    ScopeTemplate, ScriptExecutable, SourceProviderRef, TypeFlags, Value, ValueVec, Visitor, Vm,
};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(u8)]
pub enum Tier {
    Interpreter,
    Baseline,
    QuickOptimized,
    FullyOptimized,
}

/// A call that optimized code has inlined.
#[derive(Clone, Copy)]
pub struct InlineFrame {
    pub code_block: Gc<CodeBlock>,
    /// Where it was called from.
    pub caller: CodeOrigin,
    pub is_tail_call: bool,
    pub specialization: Specialization,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CodeOrigin {
    pub bytecode_offset: u32,
    /// `None` in the function that has the frame.
    pub inline_frame: Option<u32>,
}

/// Machine code for a code block, as far as the rest of the engine has to know it.
pub trait CompiledCode: GcFree + Any {
    /// What the call site slot of a frame stands for.
    fn origin_of_call_site(&self, call_site: u32) -> CodeOrigin;

    /// Whether `origin_of_call_site` knows the number, which may be anything.
    fn has_call_site(&self, call_site: u32) -> bool;

    /// What the instruction at the address, which may be anything, was made for.
    fn origin_of_address(&self, address: usize) -> Option<CodeOrigin>;

    fn inline_frame(&self, vm: &Vm, index: u32) -> InlineFrame;

    /// # Safety
    /// `frame` runs this code and is at a call site inside the inlined call.
    unsafe fn inline_callee(&self, frame: &crate::CallFrame, index: u32) -> Value;

    /// `this` and the arguments.
    ///
    /// # Safety
    /// As above.
    unsafe fn inline_arguments(&self, frame: &crate::CallFrame, index: u32, out: &mut ValueVec);

    /// Where to go on with an exception that the handler at `target` in the code of `origin` catches.
    fn catch_address(&self, call_site: u32, origin: CodeOrigin, target: u32) -> usize;

    fn visit(&self, owner: &CodeBlock, visitor: &mut Visitor);

    fn reconcile_weak_references(&self, owner: &CodeBlock, survivors: &Survivors);

    /// Makes frames that are running the code leave it at the next opportunity.
    fn invalidate(&self);

    fn size(&self) -> usize;
}

/// How often a range of the source has run.
pub type CoverageCount = Rc<Cell<u64>>;

/// The constants, the executables of inner functions, the scope templates, the profiles of the arguments and the
/// metadata.
struct Words {
    first: *mut u64,
    count: usize,
}

impl Words {
    fn layout(count: usize) -> Layout {
        Layout::array::<u64>(count.max(1)).expect("as many as the code has operands")
    }

    fn zeroed(count: usize) -> Option<Words> {
        // SAFETY: the size is not zero.
        let first = unsafe { std::alloc::alloc_zeroed(Self::layout(count)) };
        let first = NonNull::new(first)?.cast().as_ptr();
        Some(Words { first, count })
    }
}

impl Drop for Words {
    fn drop(&mut self) {
        // SAFETY: from `zeroed`.
        unsafe { std::alloc::dealloc(self.first.cast(), Self::layout(self.count)) }
    }
}

/// Code linked to a realm. Optimized code has one of its own, whose `alternative` has the profiles.
#[cell(cell_type = CodeBlock, flags = TypeFlags::SHAPE_IS_IMMORTAL, export = "engine_class_CodeBlock",
    visit_children, visit_output_constraints, reconcile_weak_references, estimated_size)]
pub struct CodeBlock {
    base: JsCell,
    instructions: *const u8,
    metadata: *mut u64,
    constants: *mut u64,
    locals: u32,
    parameters: u32,
    global: Gc<GlobalObject>,
    identifiers: *const Identifier,
    functions: *mut u64,
    scopes: *mut u64,
    executable: Gc<ScriptExecutable>,
    /// Counts up to zero, where the next tier is considered.
    execution_counter: Cell<i32>,
    tier: Cell<Tier>,
    is_jettisoned: Cell<bool>,
    saved_words: u32,
    suspend_points: *const SuspendPoint,
    saved_registers: *const Register,
    /// How many breakpoints may be hit in the code.
    debugger_requests: Cell<u32>,
    alternative: Option<Gc<CodeBlock>>,
    code: Rc<Code>,
    words: Words,
    compiled: RefCell<Option<Rc<dyn CompiledCode>>>,
    /// One for each of the coverage blocks.
    coverage_counts: Box<[CoverageCount]>,
    pub(crate) caches: metadata::SideTables,
    pub(super) optimization: Optimization,
}

impl CodeBlock {
    pub const OFFSET_OF_INSTRUCTIONS: usize = offset_of!(CodeBlock, instructions);
    pub const OFFSET_OF_METADATA: usize = offset_of!(CodeBlock, metadata);
    pub const OFFSET_OF_CONSTANTS: usize = offset_of!(CodeBlock, constants);
    pub const OFFSET_OF_LOCALS: usize = offset_of!(CodeBlock, locals);
    pub const OFFSET_OF_PARAMETERS: usize = offset_of!(CodeBlock, parameters);
    pub const OFFSET_OF_GLOBAL: usize = offset_of!(CodeBlock, global);
    pub const OFFSET_OF_IDENTIFIERS: usize = offset_of!(CodeBlock, identifiers);
    pub const OFFSET_OF_FUNCTIONS: usize = offset_of!(CodeBlock, functions);
    pub const OFFSET_OF_SCOPES: usize = offset_of!(CodeBlock, scopes);
    pub const OFFSET_OF_EXECUTION_COUNTER: usize = offset_of!(CodeBlock, execution_counter);
    pub const OFFSET_OF_DEBUGGER_REQUESTS: usize = offset_of!(CodeBlock, debugger_requests);

    /// In the realm of `cx`.
    pub fn link(
        cx: &mut Cx,
        executable: Gc<ScriptExecutable>,
        code: Rc<Code>,
    ) -> JsResult<Gc<CodeBlock>> {
        let functions = code.constants.len();
        let scopes = functions + code.functions.len();
        let metadata = scopes + code.scopes.len() + code.body.parameters() as usize;
        let count = metadata + code.body.metadata().words() as usize;
        let words = Words::zeroed(count).ok_or_else(|| cx.throw_out_of_memory())?;
        let at = |index| words.first.wrapping_add(index);
        let coverage_counts = if code.body.flags().contains(CodeFlags::HAS_COVERAGE) {
            (cx.hooks().cover)(cx, &executable, &code)
        } else {
            Box::default()
        };
        let code_block = cx.allocate::<CodeBlock>(CodeBlockInit {
            base: cx.shapes.code_block(),
            instructions: code.body.instructions().as_ptr(),
            metadata: at(metadata),
            constants: at(0),
            locals: code.body.locals(),
            parameters: code.body.parameters(),
            global: cx.global(),
            identifiers: code.identifiers.as_ptr(),
            functions: at(functions),
            scopes: at(scopes),
            executable,
            execution_counter: Cell::new(-crate::options().threshold_for_baseline),
            tier: Cell::new(Tier::Interpreter),
            is_jettisoned: Cell::new(false),
            saved_words: code.body.saved_words(),
            suspend_points: code.body.suspend_points().as_ptr(),
            saved_registers: code.body.all_saved_registers().as_ptr(),
            debugger_requests: Cell::new(0),
            alternative: None,
            code,
            words,
            compiled: RefCell::new(None),
            coverage_counts,
            caches: metadata::SideTables::default(),
            optimization: Optimization::default(),
        });
        if !code_block.coverage_counts.is_empty() {
            code_block.link_coverage_counts();
        }
        let anchored = code_block.anchor();
        for (slot, constant) in anchored.fields().iter().zip(&anchored.code.constants) {
            let value = code_block.link_constant(cx, constant)?;
            // SAFETY: traced by `visit_children`.
            unsafe { slot.set(&code_block, value) };
        }
        cx.heap
            .report_extra_memory_allocated(&code_block, count * size_of::<u64>());
        Ok(code_block)
    }

    /// For optimized code. It shares everything with `self` but the tier.
    pub fn create_replacement(&self, vm: &Vm, tier: Tier) -> Gc<CodeBlock> {
        vm.allocate::<CodeBlock>(CodeBlockInit {
            base: vm.shapes.code_block(),
            instructions: self.instructions,
            metadata: self.metadata,
            constants: self.constants,
            locals: self.locals,
            parameters: self.parameters,
            global: self.global,
            identifiers: self.identifiers,
            functions: self.functions,
            scopes: self.scopes,
            executable: self.executable,
            execution_counter: Cell::new(0),
            tier: Cell::new(tier),
            is_jettisoned: Cell::new(false),
            saved_words: self.saved_words,
            suspend_points: self.suspend_points,
            saved_registers: self.saved_registers,
            debugger_requests: Cell::new(0),
            alternative: Some(self.baseline()),
            code: Rc::clone(&self.code),
            words: Words::zeroed(0).unwrap_or_else(|| bun_alloc::out_of_memory()),
            compiled: RefCell::new(None),
            coverage_counts: Box::default(),
            caches: metadata::SideTables::default(),
            optimization: Optimization::default(),
        })
    }

    #[cold]
    fn link_coverage_counts(&self) {
        let code = self.body().instructions();
        for (_, instruction) in Instructions::new(code, 0..code.len() as u32) {
            if let (Opcode::Cover, &[block]) = (instruction.opcode, instruction.words()) {
                let count = &self.coverage_counts[block as usize];
                // SAFETY: the type of its entries, and the count goes no sooner than the metadata.
                unsafe { self.metadata::<metadata::Cover>(instruction.metadata).link(count) };
            }
        }
    }

    fn link_constant(&self, cx: &mut Cx, constant: &Constant) -> JsResult<Option<Value>> {
        Ok(Some(match constant {
            Constant::Empty => return Ok(None),
            Constant::Undefined => Value::UNDEFINED,
            Constant::Null => Value::NULL,
            &Constant::Boolean(value) => value.into(),
            &Constant::Int32(value) => Value::int32(value),
            &Constant::Double(value) => Value::double(value),
            Constant::String(atom) => JsString::from_name(cx, atom.name()).into(),
            Constant::LongString(string) => JsString::new(cx, string.clone()).into(),
            Constant::BigInt(literal) => {
                use bun_engine_bigint::LiteralRadix;
                let radix = match literal.radix {
                    2 => LiteralRadix::Binary,
                    8 => LiteralRadix::Octal,
                    16 => LiteralRadix::Hexadecimal,
                    _ => LiteralRadix::Decimal,
                };
                let digits = literal.digits.text();
                JsBigInt::from_literal(cx, digits, radix, literal.is_negative)?.into()
            }
            Constant::RegExp(literal) => {
                RegExp::for_literal(cx, literal.pattern.string(), literal.flags.text()).into()
            }
            Constant::Template(site) => self.template_object(cx, site)?.into(),
            Constant::Array(literal) => self.link_array(cx, literal)?.into(),
            &Constant::LinkTime(id) => cx.global().slot(cx, RealmSlot::for_constant(id)).into(),
        }))
    }

    fn link_array(&self, cx: &mut Cx, literal: &ArrayLiteral) -> JsResult<Gc<ImmutableArray>> {
        let mut values = ValueVec::new();
        match literal {
            ArrayLiteral::Int32(values) => {
                return cx.or_out_of_memory(ImmutableArray::try_from_int32s(cx, values));
            }
            ArrayLiteral::Double(values) => {
                return cx.or_out_of_memory(ImmutableArray::try_from_doubles(cx, values));
            }
            ArrayLiteral::Strings(atoms) => {
                for atom in atoms {
                    values.push(JsString::from_name(cx, atom.name()).into());
                }
            }
            ArrayLiteral::Values(constants) => {
                for constant in constants {
                    values.push(self.link_constant(cx, constant)?.unwrap_or_default());
                }
            }
        }
        cx.or_out_of_memory(ImmutableArray::try_from_values(cx, &values))
    }

    /// GetTemplateObject
    fn template_object(&self, cx: &mut Cx, site: &TemplateSite) -> JsResult<Gc<JsArray>> {
        let top_level = self.executable.top_level();
        if let Some(object) = top_level.template_objects().get(&site.end) {
            return Ok(object);
        }
        let (mut cooked, mut raw) = (ValueVec::new(), ValueVec::new());
        for (cooked_text, raw_text) in site.cooked.iter().zip(&site.raw) {
            let cooked_text = cooked_text.as_ref();
            cooked.push(cooked_text.map_or_default(|it| JsString::from_name(cx, it.name()).into()));
            raw.push(JsString::from_name(cx, raw_text.name()).into());
        }
        let (object, raw) = (cx.new_array_from(&cooked)?, cx.new_array_from(&raw)?);
        raw.restrict_ordinary(cx, IntegrityLevel::Frozen);
        object.put_direct(cx, names::raw, raw.into(), Attributes::CONSTANT);
        object.restrict_ordinary(cx, IntegrityLevel::Frozen);
        top_level.template_objects().insert(site.end, object);
        Ok(object)
    }

    /// They end where the metadata begins. The first is that of `this`.
    pub fn argument_profiles(&self) -> &[ValueProfile] {
        let count = self.parameters as usize;
        // SAFETY: allocated with that many.
        unsafe { core::slice::from_raw_parts(self.metadata.sub(count).cast(), count) }
    }

    /// The offset of the instruction that the call site of a frame stands for, unless its code is optimized.
    #[inline(always)]
    pub fn bytecode_offset_of(&self, call_site: u32) -> u32 {
        call_site.wrapping_sub(self.instructions.addr() as u32)
    }

    /// The low bits of the address of the instruction.
    #[inline(always)]
    pub fn call_site_of(&self, bytecode_offset: u32) -> u32 {
        bytecode_offset.wrapping_add(self.instructions.addr() as u32)
    }

    /// All words before the profiles. None for optimized code.
    fn fields(&self) -> &[Field<Option<Value>>] {
        if self.alternative.is_some() {
            return &[];
        }
        let count = (self.metadata.addr() - self.constants.addr()) / size_of::<u64>()
            - self.parameters as usize;
        // SAFETY: each is zero or a value.
        unsafe { core::slice::from_raw_parts(self.constants.cast(), count) }
    }

    #[inline(always)]
    pub fn code(&self) -> &Rc<Code> {
        &self.code
    }

    #[inline(always)]
    pub fn body(&self) -> &Body {
        &self.code.body
    }

    #[inline(always)]
    pub fn instructions_ptr(&self) -> *const u8 {
        self.instructions
    }

    #[inline(always)]
    pub fn metadata_ptr(&self) -> *mut u64 {
        self.metadata
    }

    #[inline(always)]
    pub fn constants_ptr(&self) -> *const u64 {
        self.constants
    }

    #[inline(always)]
    pub fn locals(&self) -> u32 {
        self.locals
    }

    /// The most registers that it saves when it suspends.
    #[inline(always)]
    pub fn saved_words(&self) -> u32 {
        self.saved_words
    }

    /// # Safety
    /// The code has that point.
    #[inline(always)]
    pub unsafe fn suspend_point_unchecked(&self, point: SuspendPointIndex) -> &SuspendPoint {
        debug_assert!(point.index() < self.body().suspend_points().len());
        // SAFETY: the caller's. The body is kept alive.
        unsafe { &*self.suspend_points.add(point.index()) }
    }

    /// # Safety
    /// As above.
    #[inline(always)]
    pub unsafe fn saved_registers_unchecked(&self, point: SuspendPointIndex) -> &[Register] {
        // SAFETY: as above, and the range has been verified.
        unsafe {
            let range = self.suspend_point_unchecked(point).registers();
            let first = self.saved_registers.add(range.start);
            core::slice::from_raw_parts(first, range.end - range.start)
        }
    }

    /// With `this`.
    #[inline(always)]
    pub fn parameters(&self) -> u32 {
        self.parameters
    }

    #[inline(always)]
    pub fn global(&self) -> Gc<GlobalObject> {
        self.global
    }

    #[inline(always)]
    pub fn executable(&self) -> Gc<ScriptExecutable> {
        self.executable
    }

    pub fn specialization(&self) -> Specialization {
        match self.body().kind() {
            CodeKind::Construct => Specialization::Construct,
            _ => Specialization::Call,
        }
    }

    /// The one that has the profiles and that the interpreter can run.
    #[inline(always)]
    pub fn baseline(&self) -> Gc<CodeBlock> {
        self.alternative.unwrap_or_else(|| self.into())
    }

    #[inline(always)]
    pub fn tier(&self) -> Tier {
        self.tier.get()
    }

    #[inline(always)]
    pub fn is_optimized(&self) -> bool {
        self.alternative.is_some()
    }

    #[inline(always)]
    pub fn execution_counter(&self) -> &Cell<i32> {
        &self.execution_counter
    }

    #[inline(always)]
    pub fn debugger_requests(&self) -> &Cell<u32> {
        &self.debugger_requests
    }

    pub fn compiled(&self) -> Option<Rc<dyn CompiledCode>> {
        self.compiled.borrow().clone()
    }

    pub fn set_compiled(&self, tier: Tier, compiled: Rc<dyn CompiledCode>) {
        self.tier.set(tier);
        self.compiled.replace(Some(compiled));
        self.write_barrier_for_cell();
    }

    pub fn is_jettisoned(&self) -> bool {
        self.is_jettisoned.get()
    }

    /// Optimized code whose assumptions no longer hold gives way to its alternative.
    pub fn jettison(&self, vm: &Vm) {
        if self.is_optimized() && !self.is_jettisoned.replace(true) {
            (vm.hooks().jettison)(vm, self);
        }
    }

    #[inline(always)]
    pub fn identifier(&self, index: IdentifierIndex) -> PropertyName<'_> {
        self.code.identifiers[index.index()].name()
    }

    pub fn constant(&self, index: u32) -> Option<Value> {
        self.baseline().fields()[..self.code.constants.len()][index as usize].get()
    }

    /// By the same elements in a more general kind. Code that has the address of the old one keeps that alive itself.
    pub fn replace_array_constant(&self, index: u32, elements: Gc<ImmutableArray>) {
        let owner = self.baseline();
        let field = &owner.fields()[..self.code.constants.len()][index as usize];
        assert!(field.get().is_some_and(Value::is::<ImmutableArray>));
        // SAFETY: one of its own.
        unsafe { field.set(&owner, Some(elements.into())) };
    }

    /// # Safety
    /// There is such a function.
    #[inline(always)]
    pub unsafe fn function_if_created(&self, index: FunctionIndex) -> Option<Gc<FunctionExecutable>> {
        // SAFETY: the caller's, and only executables are stored there.
        unsafe { (*self.functions.add(index.index()).cast::<Field<Option<_>>>()).get() }
    }

    /// # Safety
    /// There is such a scope.
    #[inline(always)]
    pub unsafe fn scope_template_if_created(&self, index: ScopeIndex) -> Option<Gc<ScopeTemplate>> {
        // SAFETY: the caller's, and only templates are stored there.
        unsafe { (*self.scopes.add(index.index()).cast::<Field<Option<_>>>()).get() }
    }

    #[inline(always)]
    pub fn function(&self, vm: &Vm, index: FunctionIndex) -> Gc<FunctionExecutable> {
        assert!(index.index() < self.code.functions.len());
        // SAFETY: inside, and only executables are stored there.
        let slot: &Field<Option<_>> = unsafe { &*self.functions.add(index.index()).cast() };
        slot.get().unwrap_or_else(|| {
            let info = FunctionRef::new(Rc::clone(&self.code.functions), index.0);
            let provider = self.executable.provider().clone();
            let top_level = Some(self.executable.top_level());
            let executable = FunctionExecutable::create(vm, provider, top_level, info);
            // SAFETY: the baseline traces it.
            unsafe { slot.set(&self.baseline(), Some(executable)) };
            executable
        })
    }

    #[inline(always)]
    pub fn scope_template(&self, vm: &Vm, index: ScopeIndex) -> Gc<ScopeTemplate> {
        assert!(index.index() < self.code.scopes.len());
        // SAFETY: inside, and only templates are stored there.
        let slot: &Field<Option<_>> = unsafe { &*self.scopes.add(index.index()).cast() };
        slot.get().unwrap_or_else(|| {
            let template = ScopeTemplate::create(vm, Rc::clone(&self.code.scopes[index.index()]));
            if self.has_debug_hooks() {
                template.set_owner(Some(self.executable));
            }
            // SAFETY: as above.
            unsafe { slot.set(&self.baseline(), Some(template)) };
            template
        })
    }

    /// In the text of `source_provider`.
    pub fn position(&self, bytecode_offset: u32) -> Position {
        let found = self.body().positions().find(bytecode_offset);
        found.unwrap_or_else(|| {
            let function = self.executable.downcast::<FunctionExecutable>();
            let start = function.map_or(0, |function| function.parameters_start());
            Position {
                divot: start,
                start,
                end: start,
            }
        })
    }

    pub fn source_provider(&self) -> SourceProviderRef {
        self.executable.code_provider()
    }

    fn visit_children(&self, visitor: &mut Visitor) {
        crate::Trace::trace(self.fields(), visitor);
        visitor.report_extra_memory(self.words.count * size_of::<u64>());
        if !self.is_optimized() {
            metadata::visit(self, visitor);
        }
        // SAFETY: not replaced while a marker may be running.
        if let Some(compiled) = unsafe { &*self.compiled.as_ptr() } {
            compiled.visit(self, visitor);
        }
        visitor.reconcile_current_cell_later();
    }

    fn visit_output_constraints(&self, visitor: &mut Visitor) {
        metadata::visit_transitions(self, visitor);
    }

    fn reconcile_weak_references(&self, survivors: &Survivors) {
        if !self.is_optimized() {
            metadata::reconcile_weak_references(self, survivors);
        }
        if let Some(compiled) = self.compiled() {
            compiled.reconcile_weak_references(self, survivors);
        }
    }

    fn estimated_size(&self, _: &Vm) -> usize {
        self.cell_size()
            + self.words.count * size_of::<u64>()
            + self.compiled().map_or(0, |compiled| compiled.size())
    }
}
