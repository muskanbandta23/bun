//! Code that has an address, and what is needed to walk its frames.

use core::num::NonZeroUsize;
use core::ops::RangeInclusive;
use core::ptr;
use std::sync::Arc;

use bun_engine_exec_memory::{
    CodeHandle, OutOfExecutableMemory, Owner, Priority, link, owner_of_running_code,
    set_transfer_target,
};
use bun_engine_wasm_compiler::{CallSite, CompiledFunction, InlineFrame};
use bun_engine_wasm_format::{CodeIndex, FuncIndex};
use bun_threading::Guarded;

use crate::engine::Engine;
use crate::module::Module;

const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    Baseline,
    Optimized,
    LoopEntry(u32),
}

pub(crate) struct InstalledCode {
    /// First, so that nobody finds the rest when it is dropped.
    handle: CodeHandle,
    engine: &'static Engine,
    func: FuncIndex,
    entry_offset: u32,
    frame_size: u32,
    call_sites: Box<[CallSite]>,
    inline_frames: Box<[InlineFrame]>,
    inline_ranges: Box<[(u32, u32)]>,
    /// For baseline code, by the index of the loop: what may have taken over its frames.
    loop_entries: Guarded<Vec<(u32, Arc<InstalledCode>)>>,
}

impl InstalledCode {
    pub(super) fn new(
        module: &Module,
        code: CodeIndex,
        compiled: &CompiledFunction,
        kind: Kind,
    ) -> Result<Arc<Self>, OutOfExecutableMemory> {
        let priority = match kind {
            Kind::Baseline => Priority::Required,
            Kind::Optimized | Kind::LoopEntry(_) => Priority::Optional,
        };
        let installed = Arc::new(Self {
            handle: link(&compiled.code, priority, |_| unreachable!())?,
            engine: module.engine(),
            func: module.func_index(code),
            entry_offset: compiled.entry_offset,
            frame_size: compiled.frame_size,
            call_sites: compiled.call_sites.as_slice().into(),
            inline_frames: compiled.inline_frames.as_slice().into(),
            inline_ranges: compiled.inline_ranges.as_slice().into(),
            loop_entries: Guarded::default(),
        });
        let owner = Arc::as_ptr(&installed).expose_provenance() | Owner::WASM_FUNCTION;
        let owner = NonZeroUsize::new(owner).expect("a kind");
        installed.handle.set_owner(Owner(owner));
        installed.handle.describe(|| installed.name(module, kind));
        Ok(installed)
    }

    fn name(&self, module: &Module, kind: Kind) -> String {
        let name = module.names().and_then(|names| names.func(self.func));
        let name = name.and_then(|name| core::str::from_utf8(module.bytes(name)).ok());
        format!("wasm[{}] {} {kind:?}", self.func, name.unwrap_or_default())
    }

    /// Takes no lock and allocates nothing.
    ///
    /// # Safety
    /// If the address is in the memory for generated code, a thread runs the code there or will return to it, and
    /// does not leave it during `'a`.
    pub(crate) unsafe fn running_at<'a>(address: usize) -> Option<&'a Self> {
        // SAFETY: the caller's.
        let owner = unsafe { owner_of_running_code(address) }?.0.get();
        let is_ours = owner & Owner::KIND_MASK == Owner::WASM_FUNCTION;
        // SAFETY: `new` made the word, and this lives as long as the code.
        is_ours.then(|| unsafe { &*ptr::with_exposed_provenance(owner & !Owner::KIND_MASK) })
    }

    pub(crate) fn engine(&self) -> &'static Engine {
        self.engine
    }

    pub(crate) fn func(&self) -> FuncIndex {
        self.func
    }

    pub(super) fn entry(&self) -> usize {
        self.handle.address(self.entry_offset)
    }

    pub(crate) fn frame_size(&self) -> usize {
        self.frame_size as usize
    }

    fn call_site(&self, offset: u32) -> Option<&CallSite> {
        let found = self
            .call_sites
            .binary_search_by_key(&offset, |site| site.return_offset);
        Some(&self.call_sites[found.ok()?])
    }

    fn offset_of(&self, pc: usize) -> u32 {
        (pc - self.handle.start()) as u32
    }

    pub(crate) fn handler(&self, return_address: usize) -> Option<usize> {
        let site = self.call_site(self.offset_of(return_address))?;
        (site.handler_offset != NONE).then(|| self.handle.address(site.handler_offset))
    }

    /// The innermost first.
    pub(crate) fn functions_at(&self, pc: usize) -> impl Iterator<Item = FuncIndex> {
        let offset = self.offset_of(pc);
        let innermost = self.call_site(offset).map_or_else(
            || {
                let after = self.inline_ranges.partition_point(|range| range.0 < offset);
                after
                    .checked_sub(1)
                    .map_or(NONE, |index| self.inline_ranges[index].1)
            },
            |site| site.inline_frame,
        );
        let frame = |index: u32| self.inline_frames.get(index as usize);
        core::iter::successors(frame(innermost), move |inner| frame(inner.caller))
            .map(|inlined| inlined.func)
            .chain([self.func])
    }

    /// If it fails, the call goes on through the slot.
    ///
    /// # Safety
    /// `end_offset` is that of a direct call of the function that `callee` is the optimized code of, all threads are
    /// ready to run `callee`, it outlives all runs of `self`, and no other thread changes the same call.
    pub(super) unsafe fn point_call_at(&self, end_offset: u32, callee: &Self) {
        // SAFETY: the caller's.
        let _ = unsafe { set_transfer_target(self.handle.address(end_offset), callee.entry()) };
    }

    pub(super) fn add_loop_entry(&self, loop_index: u32, code: Arc<Self>) {
        self.loop_entries.lock().push((loop_index, code));
    }

    /// The address and the size of the frame.
    pub(super) fn loop_entry(&self, loop_index: u32) -> Option<(usize, usize)> {
        let entries = self.loop_entries.lock();
        let (_, code) = entries.iter().find(|entry| entry.0 == loop_index)?;
        Some((code.entry(), code.frame_size()))
    }

    /// The addresses that a stack has if it needs this code or what this keeps.
    pub(super) fn for_each_range(&self, mut visit: impl FnMut(RangeInclusive<usize>)) {
        let range = |code: &Self| code.handle.start()..=code.handle.start() + code.handle.len();
        visit(range(self));
        for (_, entry) in &*self.loop_entries.lock() {
            visit(range(entry));
        }
    }
}
