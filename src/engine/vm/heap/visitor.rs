use core::ffi::c_void;
use core::sync::atomic::{AtomicU64, Ordering, fence};

use super::block::Version;
use super::container::{self, Container};
use super::marking::Marking;
use super::{CellState, Gc, HeapAnalyzer, RootReason, Scope, barrier};
use crate::{CellKind, JsCell, ShapeId, Value, object};

/// An entry of a mark stack with this bit is a cell whose memory has been counted by an earlier visit.
pub(super) const REVISIT: usize = 1;

/// What a marker has found out besides marks.
#[derive(Default)]
pub(super) struct Findings {
    pub(super) visit_count: usize,
    pub(super) bytes_marked: usize,
    pub(super) extra_memory: usize,
    pub(super) weak_words: Vec<usize>,
    pub(super) to_reconcile: Vec<usize>,
    pub(super) output_constraints: Vec<usize>,
}

impl Findings {
    pub(super) fn merge(&mut self, other: &mut Findings) {
        self.visit_count += core::mem::take(&mut other.visit_count);
        self.bytes_marked += core::mem::take(&mut other.bytes_marked);
        self.extra_memory = self
            .extra_memory
            .saturating_add(core::mem::take(&mut other.extra_memory));
        self.weak_words.append(&mut other.weak_words);
        self.to_reconcile.append(&mut other.to_reconcile);
        self.output_constraints
            .append(&mut other.output_constraints);
    }
}

/// Marks cells. Each marking thread has one.
pub struct Visitor {
    pub(super) stack: Vec<usize>,
    version: Version,
    is_first_visit: bool,
    current_cell: usize,
    analyzer: *mut HeapAnalyzer,
    last_opaque_root: *const c_void,
    marking: *const Marking,
    pub(super) findings: Findings,
}

impl Visitor {
    pub(super) fn new(marking: &Marking, stack: Vec<usize>) -> Visitor {
        Visitor {
            stack,
            version: marking.version,
            is_first_visit: false,
            current_cell: 0,
            analyzer: marking.analyzer,
            last_opaque_root: core::ptr::null(),
            marking,
            findings: Findings::default(),
        }
    }

    #[inline(always)]
    pub(super) fn marking(&self) -> &Marking {
        // SAFETY: a visitor does not outlive its collection.
        unsafe { &*self.marking }
    }

    pub fn scope(&self) -> Scope {
        self.marking().scope
    }

    /// What has been marked stays so while this is the same.
    pub fn epoch(&self) -> u32 {
        self.version
    }

    #[inline(always)]
    pub fn append<T: CellKind>(&mut self, cell: Gc<T>) {
        self.append_cell(cell.address());
    }

    #[inline(always)]
    pub fn append_value(&mut self, value: Value) {
        if value.is_cell() {
            self.append_cell(value.to_bits() as usize);
        }
    }

    /// # Safety
    /// Zero, or the bits of a value whose cell has not been freed.
    #[inline(always)]
    pub unsafe fn append_word(&mut self, word: u64) {
        if word & Value::NOT_CELL_MASK == 0 && word != 0 {
            self.append_cell(word as usize);
        }
    }

    #[inline(always)]
    pub(crate) fn append_shape(&mut self, id: ShapeId) {
        self.append_cell(id.address());
    }

    #[inline(always)]
    fn append_cell(&mut self, cell: usize) {
        if !self.analyzer.is_null() {
            self.analyze_edge(cell);
        }
        self.append_hidden_cell(cell);
    }

    /// An edge that a heap snapshot learns of in another way, or not at all.
    ///
    /// # Safety
    /// As for `append_word`.
    #[inline(always)]
    pub unsafe fn append_hidden_word(&mut self, word: u64) {
        if word & Value::NOT_CELL_MASK == 0 && word != 0 {
            self.append_hidden_cell(word as usize);
        }
    }

    #[inline(always)]
    pub(super) fn append_hidden_cell(&mut self, cell: usize) {
        if !self.test_and_set_marked(cell) {
            // SAFETY: what is appended has not been freed.
            unsafe { Gc::<JsCell>::from_address(cell) }.set_state(CellState::PossiblyGrey);
            self.stack.push(cell);
        }
    }

    #[inline(always)]
    fn test_and_set_marked(&mut self, cell: usize) -> bool {
        let (was_marked, container) = match container::of(cell) {
            container @ Container::Block(block) => {
                (block.test_and_set_marked(self.version, cell), container)
            }
            container @ Container::Precise(allocation) => {
                (allocation.test_and_set_marked(), container)
            }
        };
        if !was_marked {
            self.findings.bytes_marked += container.cell_size();
        }
        was_marked
    }

    /// # Safety
    /// The start of an allocation of the auxiliary space that has not been freed.
    #[inline(always)]
    pub unsafe fn mark_auxiliary(&mut self, base: usize) {
        self.test_and_set_marked(base);
    }

    #[inline]
    pub fn is_marked<T>(&self, cell: Gc<T>) -> bool {
        self.is_address_marked(cell.address())
    }

    #[inline]
    pub(crate) fn is_address_marked(&self, cell: usize) -> bool {
        match container::of(cell) {
            Container::Block(block) => block.is_marked(self.version, cell),
            Container::Precise(allocation) => allocation.is_marked(),
        }
    }

    /// The word becomes zero if the cell in it is not marked when marking ends.
    ///
    /// # Safety
    /// The word is zero or the address of a cell, and stays where it is until this collection ends.
    pub unsafe fn append_weak_word(&mut self, word: &AtomicU64) {
        let cell = word.load(Ordering::Relaxed) as usize;
        if cell != 0 && !self.is_address_marked(cell) {
            self.findings
                .weak_words
                .push(word.as_ptr().expose_provenance());
        }
    }

    /// Has `reconcile_weak_references` of the cell that is being visited called when marking ends.
    pub fn reconcile_current_cell_later(&mut self) {
        self.findings.to_reconcile.push(self.current_cell);
    }

    /// Has `visit_output_constraints` of the cell that is being visited called until it marks nothing new.
    pub fn constrain_by_current_cell(&mut self) {
        self.findings.output_constraints.push(self.current_cell);
    }

    /// Memory outside the heap that the cell being visited owns.
    #[inline]
    pub fn report_extra_memory(&mut self, bytes: usize) {
        if self.is_first_visit {
            self.findings.extra_memory = self.findings.extra_memory.saturating_add(bytes);
        }
    }

    pub fn is_first_visit(&self) -> bool {
        self.is_first_visit
    }

    #[inline]
    pub fn with_current_cell_locked<R>(&mut self, body: impl FnOnce(&mut Visitor) -> R) -> R {
        if !barrier::is_fenced() || self.current_cell == 0 {
            return body(self);
        }
        // SAFETY: it is being visited.
        let cell = unsafe { Gc::<JsCell>::from_address(self.current_cell) };
        let _lock = cell.lock();
        body(self)
    }

    /// The cell was caught in the middle of a change, so it is visited again.
    #[cold]
    pub(crate) fn did_race(&mut self) {
        // SAFETY: it is being visited.
        unsafe { Gc::<JsCell>::from_address(self.current_cell) }.set_state(CellState::PossiblyGrey);
        self.marking().raced.lock().push(self.current_cell);
    }

    /// Whether the root is new.
    pub fn add_opaque_root(&mut self, root: *const c_void) -> bool {
        if root.is_null() || root == self.last_opaque_root {
            return false;
        }
        self.last_opaque_root = root;
        let is_new = self.marking().opaque_roots().lock().insert(root.addr());
        self.findings.visit_count += usize::from(is_new);
        is_new
    }

    pub fn contains_opaque_root(&self, root: *const c_void) -> bool {
        self.marking().opaque_roots().lock().contains(&root.addr())
    }

    pub fn analyzer(&mut self) -> Option<&mut HeapAnalyzer> {
        // SAFETY: with an analyzer there is one marker, and it outlives the collection.
        unsafe { self.analyzer.as_mut() }
    }

    #[cold]
    #[inline(never)]
    fn analyze_edge(&mut self, to: usize) {
        let from = self.current_cell;
        if let Some(analyzer) = self.analyzer() {
            analyzer.will_mark(from, to);
        }
    }

    /// Attributes what `body` appends to `cell`.
    pub fn with_referrer<R>(&mut self, cell: &JsCell, body: impl FnOnce(&mut Visitor) -> R) -> R {
        let old = core::mem::replace(&mut self.current_cell, cell.address());
        let result = body(self);
        self.current_cell = old;
        result
    }

    /// For C++, which restores the old one in a destructor.
    pub fn replace_referrer(&mut self, cell: usize) -> usize {
        core::mem::replace(&mut self.current_cell, cell)
    }

    /// What a heap snapshot says of the roots that are appended from now on. Returns the old one.
    pub fn replace_root_reason(&mut self, reason: RootReason) -> RootReason {
        self.analyzer().map_or(RootReason::None, |analyzer| {
            core::mem::replace(&mut analyzer.root_reason, reason)
        })
    }

    #[inline(always)]
    pub(super) fn visit(&mut self, entry: usize) {
        let address = entry & !REVISIT;
        // SAFETY: marked.
        let cell = unsafe { Gc::<JsCell>::from_address(address) };
        self.is_first_visit = entry & REVISIT == 0;
        self.current_cell = address;
        self.findings.visit_count += 1;
        cell.set_state(CellState::PossiblyBlack);
        if self.marking().is_concurrent {
            fence(Ordering::SeqCst);
        }
        object::visit_cell(cell, self);
    }

    pub(super) fn did_finish_draining(&mut self) {
        self.current_cell = 0;
        self.is_first_visit = false;
    }
}

/// Says which cells have survived the marking that just ended.
pub struct Survivors {
    pub(super) scope: Scope,
    pub(super) vm: *const crate::Vm,
}

impl Survivors {
    #[inline]
    pub fn contains<T>(&self, cell: Gc<T>) -> bool {
        self.contains_address(cell.address())
    }

    /// Of a cell that was alive when the collection began.
    #[inline]
    pub fn contains_address(&self, cell: usize) -> bool {
        container::of(cell).is_marked(cell)
    }

    /// Memory outside the heap that a survivor owns, and that its visit has not reported.
    pub fn report_extra_memory(&self, bytes: usize) {
        self.vm().heap.did_find_extra_memory(bytes);
    }

    #[inline]
    pub fn contains_value(&self, value: Value) -> bool {
        value.as_cell().is_none_or(|cell| self.contains(cell))
    }

    #[inline]
    pub fn contains_shape(&self, id: ShapeId) -> bool {
        container::of(id.address()).is_marked(id.address())
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// Nothing may be allocated.
    pub fn vm(&self) -> &crate::Vm {
        // SAFETY: the collection runs inside a call that has it.
        unsafe { &*self.vm }
    }
}
