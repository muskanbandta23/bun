//! Instantiation, in the order of the specification.

use std::rc::Rc;
use std::sync::Arc;

use bun_engine_wasm_compiler::abi::MemoryMode;
use bun_engine_wasm_format::{CanonicalType, CanonicalValType, Extern, ValType};
use bun_engine_wasm_runtime::{
    Bits, Global, HostCell, InitializeError, Instance, LinkError, Memory, Module, Owned, Reference,
    SegmentError, SharedMemory, Table, TableType, Tag,
};

use super::machine::{Failure, HostFunction, Machine};

#[derive(Clone)]
pub(crate) enum Item {
    Function(HostCell),
    /// With its type, unless it takes any.
    Host(usize, Option<CanonicalType>),
    Table(Rc<Table>),
    Memory(Rc<Memory>),
    Global(Rc<Global>),
    Tag(Rc<Tag>),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InstantiateError {
    UnknownImport,
    WrongKind,
    Link(LinkError),
    Initialize(InitializeError),
    Segment(SegmentError),
    Start(Failure),
}

impl Machine {
    pub(crate) fn host_function(
        &self,
        ty: Option<(&[ValType], &[ValType])>,
        function: Rc<HostFunction>,
    ) -> Item {
        let ty = ty.map(|(params, results)| CanonicalType::function(params, results).unwrap());
        Item::Host(self.add_function(function), ty)
    }

    fn rooted_owner(&self, owned: Owned) -> HostCell {
        let owner = self.new_owner(owned);
        self.root(owner);
        owner
    }

    pub(crate) fn new_memory(&self, ty: bun_engine_wasm_format::Memory) -> Option<Rc<Memory>> {
        let memory = Memory::new(self.store(), ty, MemoryMode::Guarded).ok()?;
        memory.set_owner(self.rooted_owner(Owned::Memory(Rc::clone(&memory))));
        Some(memory)
    }

    pub(crate) fn adopt_memory(&self, shared: SharedMemory) -> Rc<Memory> {
        let memory = Memory::from_shared(self.store(), shared);
        memory.set_owner(self.rooted_owner(Owned::Memory(Rc::clone(&memory))));
        memory
    }

    pub(crate) fn new_table(&self, ty: TableType) -> Rc<Table> {
        let table = Table::new(self.store(), ty).unwrap();
        table.set_owner(self.rooted_owner(Owned::Table(Rc::clone(&table))));
        table
    }

    pub(crate) fn new_global(&self, ty: ValType, mutable: bool, value: Bits) -> Rc<Global> {
        let global = Global::new(self.store(), CanonicalValType::plain(ty), mutable);
        global.set_owner(self.rooted_owner(Owned::Global(Rc::clone(&global))));
        // SAFETY: the tests give values of the type.
        unsafe { global.set(value) };
        global
    }

    /// The instance is rooted if this succeeds.
    pub(crate) fn instantiate(
        &self,
        module: &Arc<Module>,
        imports: &dyn Fn(&[u8], &[u8]) -> Option<Item>,
    ) -> Result<&Instance, InstantiateError> {
        let cell = self.new_instance_cell(Instance::cell_size(module));
        // SAFETY: the heap traces and destroys it.
        let instance = unsafe { Instance::create(cell, self.store(), Arc::clone(module)) };
        let instance = instance.unwrap();
        for import in module.imports() {
            let item = imports(module.bytes(import.module), module.bytes(import.field));
            let item = item.ok_or(InstantiateError::UnknownImport)?;
            self.link(instance, import.item, item)?;
        }
        instance
            .initialize()
            .map_err(InstantiateError::Initialize)?;
        instance
            .apply_segments()
            .map_err(InstantiateError::Segment)?;
        if let Some(start) = module.start() {
            let start = instance.function_reference(start).unwrap().cell().unwrap();
            self.call(start, &[]).map_err(InstantiateError::Start)?;
        }
        self.root(cell);
        Ok(instance)
    }

    fn link(&self, instance: &Instance, what: Extern, item: Item) -> Result<(), InstantiateError> {
        let linked = match (what, item) {
            // SAFETY: from an export.
            (Extern::Func(func), Item::Function(it)) => unsafe { instance.link_function(func, it) },
            (Extern::Func(func), Item::Host(number, ty)) => {
                let module = instance.module();
                let expected = module.types().rtt(module.func_type_index(func));
                if ty.is_some_and(|ty| !ty.rtt().is_subtype_of(expected)) {
                    return Err(InstantiateError::Link(LinkError::Type));
                }
                let stub = self.engine().host_call_stub();
                // SAFETY: the stub is for any type.
                unsafe { instance.link_host_function(func, stub, Reference::NULL, number) };
                Ok(())
            }
            (Extern::Table(index), Item::Table(it)) => instance.link_table(index, it),
            (Extern::Memory(index), Item::Memory(it)) => instance.link_memory(index, it),
            (Extern::Global(index), Item::Global(it)) => instance.link_global(index, it),
            (Extern::Tag(index), Item::Tag(it)) => instance.link_tag(index, it),
            _ => return Err(InstantiateError::WrongKind),
        };
        linked.map_err(InstantiateError::Link)
    }

    pub(crate) fn invoke(
        &self,
        instance: &Instance,
        name: &str,
        arguments: &[Bits],
    ) -> Result<Vec<Bits>, Failure> {
        match export(instance, name.as_bytes()) {
            Some(Item::Function(function)) => self.call(function, arguments),
            _ => panic!("there is no function {name}"),
        }
    }
}

pub(crate) fn export(instance: &Instance, name: &[u8]) -> Option<Item> {
    let module = instance.module();
    let mut exports = module.exports().iter();
    Some(
        match exports.find(|it| module.bytes(it.name) == name)?.item {
            Extern::Func(func) => {
                Item::Function(instance.function_reference(func).unwrap().cell()?)
            }
            Extern::Table(table) => Item::Table(Rc::clone(instance.table(table))),
            Extern::Memory(memory) => Item::Memory(Rc::clone(instance.memory(memory))),
            Extern::Global(global) => Item::Global(Rc::clone(instance.global(global).unwrap())),
            Extern::Tag(tag) => Item::Tag(Rc::clone(instance.tag(tag))),
        },
    )
}
