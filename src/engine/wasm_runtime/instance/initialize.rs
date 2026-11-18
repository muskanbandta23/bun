//! What a module defines.

use std::rc::Rc;

use bun_engine_wasm_compiler::abi::{GlobalPlace, MemoryMode, Trap};
use bun_engine_wasm_format::{
    GlobalIndex, MemoryIndex, OutOfMemory, TableIndex, TagIndex, TypeIndex, TypeKind,
};

use super::{Instance, set_once};
use crate::host::Owned;
use crate::memory::Memory;
use crate::table::{Table, TableType};
use crate::tag::Tag;
use crate::value::Reference;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InitializeError {
    OutOfMemory,
    /// A table is too long, or there is no memory for it.
    Table,
    /// In a constant expression.
    Constant(Trap),
}

impl From<OutOfMemory> for InitializeError {
    fn from(_: OutOfMemory) -> Self {
        Self::OutOfMemory
    }
}

impl From<Trap> for InitializeError {
    fn from(trap: Trap) -> Self {
        Self::Constant(trap)
    }
}

/// Those that are not imported, with their indices.
fn defined<T>(all: &[T], imported: u32) -> impl Iterator<Item = (u32, &T)> {
    (imported..).zip(&all[imported as usize..])
}

impl Instance {
    /// After an error the instance is of no use.
    pub fn initialize(&self) -> Result<(), InitializeError> {
        assert!(self.imports_linked.get() == self.module.imports().len());
        self.define_memories()?;
        let first_memory = self.memories().next();
        let mode = first_memory.map_or(MemoryMode::Checked, Memory::mode);
        self.module.make_code(mode)?.add_user(&self.store.retired);
        set_once(&self.mode, mode);
        self.define_tags();
        self.define_type_data()?;
        self.define_tables()?;
        self.define_globals()?;
        Ok(self.evaluate_passive_elements()?)
    }

    fn define_memories(&self) -> Result<(), OutOfMemory> {
        for (index, &ty) in defined(self.module.memories(), self.module.imported().memories) {
            let wanted = match index {
                0 => MemoryMode::Guarded,
                _ => MemoryMode::Checked,
            };
            let memory = Memory::new(&self.store, ty, wanted)?;
            memory.set_owner(self.new_owner(Owned::Memory(Rc::clone(&memory)))?);
            self.keep_memory(MemoryIndex(index), memory);
        }
        Ok(())
    }

    fn define_tags(&self) {
        for (index, &ty) in defined(self.module.tags(), self.module.imported().tags) {
            self.keep_tag(TagIndex(index), Tag::new(self.module.types().canonical(ty)));
        }
    }

    fn define_type_data(&self) -> Result<(), OutOfMemory> {
        let types = self.module.types();
        for ty in (0..types.len()).map(TypeIndex) {
            if types.kind(ty) != TypeKind::Func && types.first_index(ty) == ty {
                let data = self.engine().host().new_type_data(self, ty);
                let offset = self.module.layout().type_data(ty);
                self.write_word(offset, data.ok_or(OutOfMemory)?.address());
                self.write_barrier();
            }
        }
        Ok(())
    }

    fn define_tables(&self) -> Result<(), InitializeError> {
        for (index, declared) in defined(self.module.tables(), self.module.imported().tables) {
            let ty = TableType {
                element: self.module.types().canonical_value(declared.element.into()),
                address: declared.address,
                limits: declared.limits,
            };
            let table = Table::new(&self.store, ty).map_err(|_| InitializeError::Table)?;
            table.set_owner(self.new_owner(Owned::Table(Rc::clone(&table)))?);
            self.keep_table(TableIndex(index), table);
            if let Some(init) = declared.init {
                let value = Reference(self.evaluate(init)? as u64);
                let table = self.table(TableIndex(index));
                // SAFETY: the initializer was validated.
                unsafe { table.fill(0, value, table.len().into()) }?;
            }
        }
        Ok(())
    }

    fn define_globals(&self) -> Result<(), InitializeError> {
        for (index, declared) in defined(self.module.globals(), self.module.imported().globals) {
            let index = GlobalIndex(index);
            let value = self.evaluate(declared.init.expect("an initializer"))?;
            match self.module.layout().global(index) {
                GlobalPlace::Inline(offset) => self.set_inline_global(index, offset, value),
                GlobalPlace::Indirect(_) => drop(self.define_global(index, value)?),
            }
        }
        Ok(())
    }
}
