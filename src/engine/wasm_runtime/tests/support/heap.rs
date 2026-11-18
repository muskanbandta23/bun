//! A small collector: generational, conservative on the stack, and eager to spoil what it frees.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::AtomicU8;

use bun_engine_wasm_compiler::abi::{Callable, HostLayout};
use bun_engine_wasm_format::CanonicalType;
use bun_engine_wasm_runtime::{Engine, HostCell, Instance, Owned, Reference, Tag};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Kind {
    Instance = 1,
    Struct,
    Array,
    Function,
    Exception,
    Owner,
    TypeData,
    Opaque,
}

const KIND: i32 = 5;
const STATE: i32 = 7;
/// What the cell owns, the type data of an object, or the number of an opaque cell.
const PRIVATE: i32 = 8;
const OLD: u8 = 0;
const YOUNG: u8 = 1;
const REMEMBERED: u8 = 2;

pub(crate) static BARRIER_THRESHOLD: AtomicU8 = AtomicU8::new(OLD);

pub(crate) fn layout(vm_top_frame: i32) -> HostLayout {
    HostLayout {
        instance_header_size: 16,
        cell_kind: KIND,
        struct_kind: Kind::Struct as u8,
        array_kind: Kind::Array as u8,
        cell_state: STATE,
        barrier_threshold_address: BARRIER_THRESHOLD.as_ptr().expose_provenance(),
        object_rtt: 16,
        struct_fields: 24,
        array_length: 24,
        array_elements: 32,
        function_callable: 8,
        exception_tag: 16,
        exception_payload: 24,
        vm_top_frame,
        null_guard_size: 0,
    }
}

fn field<T>(cell: HostCell, offset: i32) -> *mut T {
    ptr::with_exposed_provenance_mut(cell.address() + offset as usize)
}

pub(crate) fn kind_of(cell: HostCell) -> Kind {
    // SAFETY: the tests ask about live cells.
    unsafe { field::<Kind>(cell, KIND).read() }
}

pub(crate) fn private_word(cell: HostCell) -> usize {
    // SAFETY: as above.
    unsafe { field::<usize>(cell, PRIVATE).read() }
}

/// # Safety
/// The cell owns a `T`, and lives for `'a`.
pub(crate) unsafe fn owned<'a, T>(cell: HostCell) -> &'a T {
    // SAFETY: the caller's.
    unsafe { &*ptr::with_exposed_provenance(private_word(cell)) }
}

const CHUNK_SIZE: usize = 1 << 20;
const ALIGNMENT: usize = 16;

#[derive(Default)]
pub(crate) struct Heap {
    /// Without it cells stay until the heap is dropped.
    pub(crate) collects: bool,
    sizes: BTreeMap<usize, usize>,
    remembered: Vec<HostCell>,
    spoiled: Vec<(usize, usize)>,
    chunks: Vec<(usize, usize)>,
    chunk_free: std::ops::Range<usize>,
    pub(crate) freed: usize,
}

fn allocate(size: usize) -> Option<usize> {
    // SAFETY: no cell is empty.
    let start = unsafe { alloc_zeroed(Layout::from_size_align(size, ALIGNMENT).ok()?) };
    (!start.is_null()).then(|| start.expose_provenance())
}

fn free((start, size): (usize, usize)) {
    let layout = Layout::from_size_align(size, ALIGNMENT).unwrap();
    // SAFETY: from `allocate`.
    unsafe { dealloc(ptr::with_exposed_provenance_mut(start), layout) };
}

impl Heap {
    pub(crate) fn new_cell(&mut self, kind: Kind, size: usize, private: usize) -> Option<HostCell> {
        let size = size.next_multiple_of(8);
        let address = if self.collects || !matches!(kind, Kind::Struct | Kind::Array) {
            let address = allocate(size)?;
            self.sizes.insert(address, size);
            address
        } else {
            if self.chunk_free.len() < size {
                let chunk_size = size.max(CHUNK_SIZE);
                let start = allocate(chunk_size)?;
                self.chunks.push((start, chunk_size));
                self.chunk_free = start..start + chunk_size;
            }
            self.chunk_free.start += size;
            self.chunk_free.start - size
        };
        let cell = HostCell(NonZeroUsize::new(address)?);
        // SAFETY: every cell has two words.
        unsafe {
            field::<Kind>(cell, KIND).write(kind);
            field::<u8>(cell, STATE).write(YOUNG);
            field::<usize>(cell, PRIVATE).write(private);
        }
        Some(cell)
    }

    pub(crate) fn new_owner<T>(&mut self, kind: Kind, size: usize, value: T) -> Option<HostCell> {
        let owned = Box::into_raw(Box::new(value)).expose_provenance();
        self.new_cell(kind, size, owned)
    }

    pub(crate) fn remember(&mut self, cell: HostCell) {
        let state = field::<u8>(cell, STATE);
        // SAFETY: a live cell.
        unsafe {
            if state.read() == OLD {
                state.write(REMEMBERED);
                self.remembered.push(cell);
            }
        }
    }

    pub(crate) fn cell_containing(&self, word: usize) -> Option<HostCell> {
        let (&start, &size) = self.sizes.range(..=word).next_back()?;
        (word < start + size).then(|| HostCell(NonZeroUsize::new(start).unwrap()))
    }

    /// Returns the cells that died, for `destroy`.
    pub(crate) fn mark(
        &mut self,
        engine: &Engine,
        roots: impl Iterator<Item = usize>,
        is_full: bool,
    ) -> Vec<HostCell> {
        let cells = || {
            self.sizes
                .keys()
                .map(|&it| HostCell(NonZeroUsize::new(it).unwrap()))
        };
        let mut pending = std::mem::take(&mut self.remembered);
        if is_full {
            pending.clear();
            // SAFETY: live cells.
            cells().for_each(|cell| unsafe { field::<u8>(cell, STATE).write(YOUNG) });
        }
        pending.extend(roots.filter_map(|word| self.cell_containing(word)));
        while let Some(cell) = pending.pop() {
            // SAFETY: as above.
            if unsafe { field::<u8>(cell, STATE).replace(OLD) } != OLD {
                trace(engine, cell, &mut |referent| {
                    assert!(
                        self.sizes.contains_key(&referent.address()),
                        "a {:?} refers to a cell that was freed",
                        kind_of(cell)
                    );
                    pending.push(referent);
                });
            }
        }
        // SAFETY: as above.
        let is_dead = |&cell: &HostCell| unsafe { field::<u8>(cell, STATE).read() } != OLD;
        cells().filter(is_dead).collect()
    }

    pub(crate) fn destroy(&mut self, engine: &Engine, cell: HostCell) {
        const KEPT_SPOILED: usize = 1 << 14;
        let size = self.sizes.remove(&cell.address()).unwrap();
        // SAFETY: nothing refers to the cell, which owns what its kind says.
        unsafe {
            let private = private_word(cell);
            let drop_box = |drop: unsafe fn(usize)| drop(private);
            match kind_of(cell) {
                Kind::Instance => Instance::destroy(engine, cell),
                Kind::Exception => drop_box(drop_owned::<Rc<Tag>>),
                Kind::Owner => drop_box(drop_owned::<Owned>),
                Kind::TypeData => drop_box(drop_owned::<CanonicalType>),
                Kind::Struct | Kind::Array | Kind::Function | Kind::Opaque => {}
            }
            field::<u8>(cell, 0).write_bytes(0xdb, size);
        }
        self.freed += 1;
        self.spoiled.push((cell.address(), size));
        if self.spoiled.len() == 2 * KEPT_SPOILED {
            self.spoiled.drain(..KEPT_SPOILED).for_each(free);
        }
    }

    pub(crate) fn destroy_all(&mut self, engine: &Engine) {
        while let Some((&address, _)) = self.sizes.first_key_value() {
            self.destroy(engine, HostCell(NonZeroUsize::new(address).unwrap()));
        }
        self.spoiled
            .drain(..)
            .chain(self.chunks.drain(..))
            .for_each(free);
    }
}

unsafe fn drop_owned<T>(address: usize) {
    // SAFETY: from `new_owner`.
    drop(unsafe { Box::from_raw(ptr::with_exposed_provenance_mut::<T>(address)) });
}

fn trace(engine: &Engine, cell: HostCell, visit: &mut dyn FnMut(HostCell)) {
    let layout = engine.layout();
    // SAFETY: the kind tells what is in the cell.
    unsafe {
        let mut visit_field = |offset: i32| {
            let value = Reference(field::<u64>(cell, offset).read());
            value.cell().into_iter().for_each(&mut *visit);
        };
        match kind_of(cell) {
            Kind::Instance => {
                if let Some(instance) = Instance::of_cell(engine, cell) {
                    instance.trace(visit);
                }
            }
            Kind::Struct | Kind::Array => {
                visit_field(PRIVATE);
                engine.trace_object(cell, visit);
            }
            Kind::Function => visit_field(layout.function_callable + Callable::INSTANCE),
            Kind::Exception => {
                for (ty, offset) in owned::<Rc<Tag>>(cell).payload() {
                    if ty.is_ref() {
                        visit_field(layout.exception_payload + offset as i32);
                    }
                }
            }
            Kind::Owner => match owned::<Owned>(cell) {
                Owned::Memory(_) => {}
                Owned::Table(table) => table.trace(visit),
                Owned::Global(global) => global.trace(visit),
            },
            Kind::TypeData | Kind::Opaque => {}
        }
    }
}
