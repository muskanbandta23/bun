use core::ffi::c_void;
use core::sync::atomic::{AtomicU64, Ordering, fence};

use super::block::{Block, UNUSED_VERSION, Version};
use super::container::{self, Container};
use super::marking::Marking;
use super::stack::prefetch;
use super::verifier::Verifier;
use super::{CellState, Gc, HeapAnalyzer, PRECISE_BIT, RootReason, Scope, WeakImpl, barrier};
use crate::{CellKind, FastHashMap, FastHashSet, JsCell, ShapeId, Value, object};

/// What an entry of a mark stack is says its lowest bits. Without any, it is a cell that has just been marked.
const KIND_MASK: usize = 7;
/// A cell whose memory has been counted by an earlier visit.
pub(super) const REVISIT: usize = 1;
/// To which the stride is added. A run of records of that many words.
const RECORDS: usize = 1;
const OUTPUT_OF_CELL: usize = 5;
/// Of the cells that are marked.
const OUTPUT_OF_BLOCK: usize = 6;
/// A run of references to them.
const WEAK_HANDLES: usize = 7;

/// A run has its length, in chunks, above the address.
const LENGTH_SHIFT: u32 = 48;
const MAX_CHUNKS: usize = (1 << (usize::BITS - LENGTH_SHIFT)) - 1;
const RECORDS_PER_CHUNK: usize = 256;
const HANDLES_PER_CHUNK: usize = 64;
/// What a chunk or a block counts for, in visits.
const WORK_PER_CHUNK: usize = 64;

const PREFETCH_DISTANCE: usize = 8;

/// What a marker has found out besides marks.
#[derive(Default)]
pub(super) struct Findings {
    pub(super) visit_count: usize,
    pub(super) precise_bytes_marked: usize,
    pub(super) extra_memory: usize,
    pub(super) prepared_blocks: Vec<usize>,
    pub(super) weak_words: Vec<usize>,
    pub(super) to_reconcile: Vec<usize>,
    pub(super) output_constraints: Vec<usize>,
    pub(super) ephemeron_tables: Vec<usize>,
    /// Those that wait until there is only one marker.
    pub(super) opaque_roots: FastHashSet<usize>,
}

impl Findings {
    pub(super) fn merge(&mut self, other: &mut Findings) {
        self.visit_count += core::mem::take(&mut other.visit_count);
        self.precise_bytes_marked += core::mem::take(&mut other.precise_bytes_marked);
        self.extra_memory = self
            .extra_memory
            .saturating_add(core::mem::take(&mut other.extra_memory));
        self.prepared_blocks.append(&mut other.prepared_blocks);
        self.weak_words.append(&mut other.weak_words);
        self.to_reconcile.append(&mut other.to_reconcile);
        self.output_constraints
            .append(&mut other.output_constraints);
        self.ephemeron_tables.append(&mut other.ephemeron_tables);
        self.opaque_roots.extend(other.opaque_roots.drain());
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Visible,
    Hidden,
}

/// Marks cells. Each marking thread has one.
pub struct Visitor {
    pub(super) stack: Vec<usize>,
    /// That of no block if each reference has to be looked at more closely.
    fast_version: Version,
    pub(super) is_alone: bool,
    is_first_visit: bool,
    current_cell: usize,
    last_opaque_root: *const c_void,
    marking: *const Marking,
    pub(super) findings: Findings,
    /// In place of the marks of the heap.
    verifier: Option<Verifier>,
    /// The values that live if the key does. `None` while the tables are gone through again and again instead.
    ephemerons: Option<FastHashMap<usize, smallvec::SmallVec<[usize; 1]>>>,
}

impl Visitor {
    pub(super) fn new(marking: &Marking, stack: Vec<usize>) -> Visitor {
        Visitor {
            stack,
            fast_version: if marking.analyzer.is_null() {
                marking.version
            } else {
                UNUSED_VERSION
            },
            is_alone: true,
            is_first_visit: false,
            current_cell: 0,
            last_opaque_root: core::ptr::null(),
            marking,
            findings: Findings::default(),
            verifier: None,
            ephemerons: None,
        }
    }

    /// `marking` has one marker.
    pub(super) fn verifying(marking: &Marking, verifier: Verifier) -> Visitor {
        Visitor {
            fast_version: UNUSED_VERSION,
            verifier: Some(verifier),
            ..Visitor::new(marking, Vec::new())
        }
    }

    #[inline(always)]
    pub(super) fn marking(&self) -> &Marking {
        // SAFETY: a visitor is not used after its collection.
        unsafe { &*self.marking }
    }

    #[inline(always)]
    fn looks_closely(&self) -> bool {
        self.fast_version == UNUSED_VERSION
    }

    pub fn scope(&self) -> Scope {
        self.marking().scope
    }

    /// What has been marked stays so while this is the same.
    pub fn epoch(&self) -> u32 {
        self.marking().version
    }

    /// Marking is over, and is being done again to check it. The scope is full, and no visit is the first.
    pub fn is_verifying(&self) -> bool {
        self.verifier.is_some()
    }

    #[inline(always)]
    pub fn append<T: CellKind>(&mut self, cell: Gc<T>) {
        self.append_cell(cell.address(), Edge::Visible);
    }

    #[inline(always)]
    pub fn append_value(&mut self, value: Value) {
        if value.is_cell() {
            self.append_cell(value.to_bits() as usize, Edge::Visible);
        }
    }

    /// # Safety
    /// Zero, or the bits of a value whose cell has not been freed.
    #[inline(always)]
    pub unsafe fn append_word(&mut self, word: u64) {
        if word & Value::NOT_CELL_MASK == 0 && word != 0 {
            self.append_cell(word as usize, Edge::Visible);
        }
    }

    #[inline(always)]
    pub(crate) fn append_shape(&mut self, id: ShapeId) {
        // SAFETY: shapes are in blocks.
        let block = unsafe { Block::containing(id.address()) };
        if !block.is_marked(self.fast_version, id.address()) {
            self.append_slow(id.address(), Edge::Visible);
        }
    }

    /// An edge that a heap snapshot learns of in another way, or not at all.
    ///
    /// # Safety
    /// As for `append_word`.
    #[inline(always)]
    pub unsafe fn append_hidden_word(&mut self, word: u64) {
        if word & Value::NOT_CELL_MASK == 0 && word != 0 {
            self.append_cell(word as usize, Edge::Hidden);
        }
    }

    pub(super) fn append_hidden_cell(&mut self, cell: usize) {
        self.append_cell(cell, Edge::Hidden);
    }

    #[inline(always)]
    fn append_cell(&mut self, cell: usize, edge: Edge) {
        if cell & PRECISE_BIT == 0 {
            // SAFETY: what is appended has not been freed.
            let block = unsafe { Block::containing(cell) };
            if block.has_marks_of(self.fast_version) {
                if !block.has_mark(cell) && self.set_mark(block, cell) {
                    self.stack.push(cell);
                }
                return;
            }
        }
        self.append_slow(cell, edge);
    }

    /// False if another marker was first.
    #[inline(always)]
    fn set_mark(&self, block: &Block, cell: usize) -> bool {
        if self.is_alone {
            block.set_mark_exclusively(cell);
            return true;
        }
        !block.test_and_set_mark(cell)
    }

    #[inline(never)]
    fn append_slow(&mut self, cell: usize, edge: Edge) {
        let from = self.current_cell;
        let is_new = if let Some(verifier) = &mut self.verifier {
            verifier.reach_cell(from, cell)
        } else {
            if edge == Edge::Visible
                && let Some(analyzer) = self.analyzer()
            {
                analyzer.edge(from, cell);
            }
            self.mark_slow(cell)
        };
        if is_new {
            self.stack.push(cell);
        }
    }

    /// Whether it was not marked.
    fn mark_slow(&mut self, cell: usize) -> bool {
        match container::of(cell) {
            Container::Block(block) => {
                let version = self.marking().version;
                if !block.has_marks_of(version) && block.prepare_for_marking(version) {
                    self.findings.prepared_blocks.push(block.address());
                }
                !block.has_mark(cell) && self.set_mark(block, cell)
            }
            Container::Precise(allocation) => {
                let is_new = !allocation.test_and_set_marked();
                if is_new {
                    self.findings.precise_bytes_marked += allocation.cell_size();
                }
                is_new
            }
        }
    }

    /// # Safety
    /// The start of an allocation of the auxiliary space that has not been freed.
    #[inline(always)]
    pub unsafe fn mark_auxiliary(&mut self, base: usize) {
        if base & PRECISE_BIT == 0 {
            // SAFETY: the caller's.
            let block = unsafe { Block::containing(base) };
            if block.has_marks_of(self.fast_version) {
                if !block.has_mark(base) {
                    self.set_mark(block, base);
                }
                return;
            }
        }
        self.mark_auxiliary_slow(base);
    }

    #[inline(never)]
    fn mark_auxiliary_slow(&mut self, base: usize) {
        match &self.verifier {
            Some(verifier) => verifier.reach_auxiliary(self.current_cell, base),
            None => {
                self.mark_slow(base);
            }
        }
    }

    /// Other markers may get a part of a long run.
    ///
    /// # Safety
    /// Each word is as for `append_word`, and the memory is that of the heap.
    #[inline(always)]
    pub unsafe fn append_hidden_words(&mut self, first: *const u64, count: usize) {
        // SAFETY: the caller's.
        unsafe { self.append_records::<1>(first, count, Edge::Hidden) }
    }

    /// The same, of records of two or three words, the second of which is no value.
    ///
    /// # Safety
    /// As above.
    #[inline(always)]
    pub(crate) unsafe fn append_entries<const WORDS: usize>(
        &mut self,
        first: *const u64,
        count: usize,
    ) {
        // SAFETY: the caller's.
        unsafe { self.append_records::<WORDS>(first, count, Edge::Visible) }
    }

    #[inline(always)]
    unsafe fn append_records<const WORDS: usize>(
        &mut self,
        first: *const u64,
        mut count: usize,
        edge: Edge,
    ) {
        if count >= 2 * RECORDS_PER_CHUNK && !self.looks_closely() {
            let (now, chunk) = (count % RECORDS_PER_CHUNK, RECORDS_PER_CHUNK * WORDS * 8);
            let rest = first.wrapping_add(now * WORDS).expose_provenance();
            self.push_run(rest | (RECORDS + WORDS), count / RECORDS_PER_CHUNK, chunk);
            count = now;
        }
        // SAFETY: the caller's.
        unsafe { self.append_records_now::<WORDS>(first, count, edge) }
    }

    #[inline(always)]
    unsafe fn append_records_now<const WORDS: usize>(
        &mut self,
        first: *const u64,
        count: usize,
        edge: Edge,
    ) {
        const { assert!(matches!(WORDS, 1..=3)) };
        for index in 0..count {
            for offset in [0, 2].into_iter().filter(|&offset| offset < WORDS) {
                // SAFETY: the caller's.
                let word =
                    unsafe { AtomicU64::from_ptr(first.add(index * WORDS + offset).cast_mut()) }
                        .load(Ordering::Relaxed);
                if word & Value::NOT_CELL_MASK == 0 && word != 0 {
                    self.append_cell(word as usize, edge);
                }
            }
        }
    }

    #[inline(never)]
    fn push_run(&mut self, mut entry: usize, mut chunks: usize, chunk_size: usize) {
        while chunks != 0 {
            let length = chunks.min(MAX_CHUNKS);
            self.stack.push(entry | length << LENGTH_SHIFT);
            entry += length * chunk_size;
            chunks -= length;
        }
    }

    /// Puts all chunks but the first back, in pieces that can be shared, and returns the address of that one.
    fn split_run(&mut self, entry: usize, chunk_size: usize) -> usize {
        let (first, mut chunks) = (entry & ((1 << LENGTH_SHIFT) - 1), entry >> LENGTH_SHIFT);
        while chunks > 1 {
            let kept = chunks / 2;
            (self.stack).push((first + kept * chunk_size) | (chunks - kept) << LENGTH_SHIFT);
            chunks = kept;
        }
        first & !KIND_MASK
    }

    fn visit_records<const WORDS: usize>(&mut self, entry: usize) {
        let first = self.split_run(entry, RECORDS_PER_CHUNK * WORDS * 8);
        let first = core::ptr::with_exposed_provenance(first);
        // SAFETY: promised to `append_records`.
        unsafe { self.append_records_now::<WORDS>(first, RECORDS_PER_CHUNK, Edge::Hidden) }
    }

    /// The slice stays as it is until the stack has been drained.
    pub(super) fn push_weak_handles(&mut self, handles: &[&'static WeakImpl]) {
        let (chunks, rest) = handles.as_chunks::<HANDLES_PER_CHUNK>();
        let first = chunks.as_ptr().expose_provenance();
        let chunk_size = size_of::<[&WeakImpl; HANDLES_PER_CHUNK]>();
        self.push_run(first | WEAK_HANDLES, chunks.len(), chunk_size);
        self.visit_weak_handles(rest);
    }

    fn visit_weak_handles(&mut self, handles: &[&WeakImpl]) {
        let reason = self.replace_root_reason(RootReason::WeakSets);
        self.did_finish_draining();
        for handle in handles {
            handle.visit(self);
        }
        self.replace_root_reason(reason);
    }

    pub(super) fn push_output_of_cells(&mut self, cells: &[usize]) {
        (self.stack).extend(cells.iter().map(|cell| cell | OUTPUT_OF_CELL));
    }

    pub(super) fn push_output_of_block(&mut self, block: &Block) {
        self.stack.push(block.address() | OUTPUT_OF_BLOCK);
    }

    fn visit_output(&mut self, address: usize, reason: RootReason) {
        let reason = self.replace_root_reason(reason);
        // SAFETY: marked.
        let cell = unsafe { Gc::<JsCell>::from_address(address) };
        (self.current_cell, self.is_first_visit) = (address, false);
        // SAFETY: the method of its class.
        unsafe { (cell.class().ops().visit_output_constraints)(&cell, self) };
        self.replace_root_reason(reason);
    }

    #[inline]
    pub fn is_marked<T>(&self, cell: Gc<T>) -> bool {
        self.is_address_marked(cell.address())
    }

    #[inline]
    pub(crate) fn is_address_marked(&self, cell: usize) -> bool {
        if let Some(verifier) = &self.verifier {
            return verifier.has_reached(cell);
        }
        match container::of(cell) {
            Container::Block(block) => block.is_marked(self.marking().version, cell),
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

    /// Has `visit_output_constraints` of the cell that is being visited called, on any thread, until it marks nothing
    /// new.
    pub fn constrain_by_current_cell(&mut self) {
        self.findings.output_constraints.push(self.current_cell);
    }

    /// The same, for a method that does nothing but `append_ephemeron`.
    pub(crate) fn constrain_by_ephemerons_of_current_cell(&mut self) {
        self.findings.ephemeron_tables.push(self.current_cell);
    }

    /// `value` lives if the cell at `key` does.
    #[inline(always)]
    pub(crate) fn append_ephemeron(&mut self, key: usize, value: Gc<JsCell>) {
        if self.is_address_marked(key) {
            self.append(value);
        } else if let Some(ephemerons) = &mut self.ephemerons {
            ephemerons.entry(key).or_default().push(value.address());
        }
    }

    /// From now on this visitor does all that is left by itself, and has to see each table only once more.
    pub(super) fn start_remembering_ephemerons(&mut self) {
        self.ephemerons.get_or_insert_default();
    }

    pub(super) fn remembers_ephemerons(&self) -> bool {
        self.ephemerons.is_some()
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

    /// Whether the root is new, as far as this marker knows.
    pub fn add_opaque_root(&mut self, root: *const c_void) -> bool {
        if root.is_null() || root == self.last_opaque_root {
            return false;
        }
        self.last_opaque_root = root;
        let is_new = if self.is_alone {
            // SAFETY: alone.
            unsafe { self.marking().opaque_roots().get_mut() }.insert(root.addr())
        } else {
            !self.marking().opaque_roots().contains(root.addr())
                && self.findings.opaque_roots.insert(root.addr())
        };
        self.findings.visit_count += usize::from(is_new);
        is_new
    }

    /// What another marker has just added may be missing, so this is for what is asked again until nothing changes.
    pub fn contains_opaque_root(&self, root: *const c_void) -> bool {
        self.marking().opaque_roots().contains(root.addr())
            || self.findings.opaque_roots.contains(&root.addr())
    }

    pub fn analyzer(&mut self) -> Option<&mut HeapAnalyzer> {
        // SAFETY: with an analyzer there is one marker, and it outlives the collection.
        unsafe { self.marking().analyzer.as_mut() }
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
    fn visit_cell(&mut self, address: usize, is_first_visit: bool, is_concurrent: bool) {
        // SAFETY: marked.
        let cell = unsafe { Gc::<JsCell>::from_address(address) };
        (self.current_cell, self.is_first_visit) = (address, is_first_visit);
        // Spares writing the memory of what has survived before.
        if cell.state() != barrier::BLACK_THRESHOLD {
            cell.set_state(CellState::PossiblyBlack);
        }
        if is_concurrent {
            fence(Ordering::SeqCst);
        }
        object::visit_cell(cell, self);
    }

    /// Anything but a first visit. Returns how much work it was.
    #[inline(never)]
    fn visit_other(&mut self, entry: usize) -> usize {
        match entry & KIND_MASK {
            REVISIT => {
                self.findings.visit_count += 1;
                self.visit_cell(entry - REVISIT, false, self.marking().is_concurrent);
                return 1;
            }
            OUTPUT_OF_CELL => {
                self.visit_output(entry - OUTPUT_OF_CELL, RootReason::Output);
                return 1;
            }
            OUTPUT_OF_BLOCK => {
                // SAFETY: from `push_output_of_block`.
                unsafe { Block::containing(entry) }.for_each_marked_cell(|cell| {
                    if self.is_address_marked(cell) {
                        self.visit_output(cell, RootReason::Embedder);
                    }
                });
            }
            WEAK_HANDLES => {
                type Chunk = [&'static WeakImpl; HANDLES_PER_CHUNK];
                let chunk = self.split_run(entry, size_of::<Chunk>());
                // SAFETY: from `push_weak_handles`.
                self.visit_weak_handles(unsafe {
                    &*core::ptr::with_exposed_provenance::<Chunk>(chunk)
                });
            }
            kind => match kind - RECORDS {
                1 => self.visit_records::<1>(entry),
                2 => self.visit_records::<2>(entry),
                _ => self.visit_records::<3>(entry),
            },
        }
        WORK_PER_CHUNK
    }

    /// Stops after about that many visits.
    #[inline(never)]
    pub(super) fn drain(&mut self, budget: usize) {
        if self.looks_closely() || self.remembers_ephemerons() {
            return self.drain_slowly();
        }
        let is_concurrent = self.marking().is_concurrent;
        // An entry waits here while its memory is on the way.
        let mut queue = [0; PREFETCH_DISTANCE];
        let (mut head, mut tail) = (0, 0);
        let (mut work, mut first_visits) = (0, 0);
        loop {
            while tail - head < PREFETCH_DISTANCE
                && work < budget
                && let Some(entry) = self.stack.pop()
            {
                prefetch(entry);
                queue[tail % PREFETCH_DISTANCE] = entry;
                tail += 1;
            }
            if head == tail {
                break;
            }
            let entry = queue[head % PREFETCH_DISTANCE];
            head += 1;
            if entry & KIND_MASK == 0 {
                self.visit_cell(entry, true, is_concurrent);
                first_visits += 1;
                work += 1;
            } else {
                work += self.visit_other(entry);
            }
        }
        self.findings.visit_count += first_visits;
    }

    #[cold]
    fn drain_slowly(&mut self) {
        while let Some(entry) = self.stack.pop() {
            if entry & KIND_MASK != 0 {
                self.visit_other(entry);
                continue;
            }
            self.findings.visit_count += 1;
            self.visit_cell(entry, !self.is_verifying(), self.marking().is_concurrent);
            let values = self.ephemerons.as_mut().and_then(|it| it.remove(&entry));
            for value in values.into_iter().flatten() {
                self.append_cell(value, Edge::Visible);
            }
            if let Some(analyzer) = self.analyzer() {
                // SAFETY: marked.
                analyzer.node(unsafe { Gc::from_address(entry) });
            }
        }
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
    pub(crate) fn contains_address(&self, cell: usize) -> bool {
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
    pub(crate) fn vm(&self) -> &crate::Vm {
        // SAFETY: the collection runs inside a call that has it.
        unsafe { &*self.vm }
    }
}
