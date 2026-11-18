//! A valid module and its code.

use std::sync::{Arc, OnceLock};

use bun_engine_wasm_compiler::abi::{InstanceLayout, MemoryMode, Signature};
use bun_engine_wasm_format::{FuncIndex, OutOfMemory};

use crate::alloc::try_defaults;
use crate::code::CodeGroup;
use crate::engine::Engine;

pub struct Module {
    engine: &'static Engine,
    contents: bun_engine_wasm_format::Module,
    layout: InstanceLayout,
    /// By type, for calls between the host and WebAssembly.
    signatures: Box<[OnceLock<Box<Signature>>]>,
    guarded: OnceLock<CodeGroup>,
    checked: OnceLock<CodeGroup>,
}

impl core::ops::Deref for Module {
    type Target = bun_engine_wasm_format::Module;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.contents
    }
}

impl Module {
    pub(crate) fn new(
        engine: &'static Engine,
        contents: bun_engine_wasm_format::Module,
    ) -> Result<Arc<Self>, OutOfMemory> {
        Ok(Arc::new(Self {
            engine,
            layout: InstanceLayout::new(&contents, engine.layout()),
            signatures: try_defaults(contents.types().len() as usize)?,
            contents,
            guarded: OnceLock::new(),
            checked: OnceLock::new(),
        }))
    }

    #[inline]
    pub fn engine(&self) -> &'static Engine {
        self.engine
    }

    #[inline]
    pub fn layout(&self) -> &InstanceLayout {
        &self.layout
    }

    pub(crate) fn signature(&self, func: FuncIndex) -> &Signature {
        let ty = self.func_type_index(func);
        let make = || Box::new(Signature::new(self.types().func_type(ty)));
        self.signatures[ty.usize()].get_or_init(make)
    }

    fn group(&self, mode: MemoryMode) -> &OnceLock<CodeGroup> {
        match mode {
            MemoryMode::Guarded => &self.guarded,
            MemoryMode::Checked => &self.checked,
        }
    }

    pub(crate) fn make_code(&self, mode: MemoryMode) -> Result<&CodeGroup, OutOfMemory> {
        if let Some(group) = self.group(mode).get() {
            return Ok(group);
        }
        let group = CodeGroup::new(self, mode)?;
        Ok(self.group(mode).get_or_init(|| group))
    }

    /// After `make_code`.
    #[inline]
    pub(crate) fn code(&self, mode: MemoryMode) -> &CodeGroup {
        self.group(mode).get().expect("an instance asked for it")
    }
}
