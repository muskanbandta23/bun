use core::ffi::c_void;

use bun_engine_text::{PropertyName, StringImpl};

use super::{Gc, JsCell, Scope, Visitor};
use crate::AsciiLiteral;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum RootReason {
    None,
    StrongReferences,
    ProtectedValues,
    StrongHandles,
    WeakSets,
    Output,
    Embedder,
    ExecutingCode,
    Debugger,
}

/// Receives the object graph from a collection. Cells are alive only until it ends.
pub trait HeapGraphSink {
    fn node(&mut self, cell: Gc<JsCell>);
    /// From a root if `from` is `None`.
    fn edge(&mut self, from: Option<Gc<JsCell>>, to: Gc<JsCell>, reason: RootReason);
    fn property_edge(&mut self, from: Gc<JsCell>, to: Gc<JsCell>, name: PropertyName<'_>);
    fn variable_edge(&mut self, from: Gc<JsCell>, to: Gc<JsCell>, name: PropertyName<'_>);
    fn index_edge(&mut self, from: Gc<JsCell>, to: Gc<JsCell>, index: u32);
    fn label(&mut self, cell: Gc<JsCell>, label: &StringImpl);
    fn wrapped_object(&mut self, cell: Gc<JsCell>, wrapped: *const c_void);
    fn opaque_root_reason(&mut self, cell: Gc<JsCell>, reason: AsciiLiteral);
}

pub struct HeapAnalyzer {
    pub(super) root_reason: RootReason,
    pub sink: Box<dyn HeapGraphSink>,
}

impl HeapAnalyzer {
    pub fn new(sink: Box<dyn HeapGraphSink>) -> HeapAnalyzer {
        HeapAnalyzer {
            root_reason: RootReason::None,
            sink,
        }
    }

    pub(super) fn node(&mut self, cell: Gc<JsCell>) {
        self.sink.node(cell);
        // SAFETY: the method of its class.
        unsafe { (cell.class().ops().analyze_heap)(&cell, self) }
    }

    pub(super) fn edge(&mut self, from: usize, to: usize) {
        // SAFETY: what is being visited, and what it refers to.
        let (from, to) = unsafe {
            (
                (from != 0).then(|| Gc::from_address(from)),
                Gc::from_address(to),
            )
        };
        self.sink.edge(from, to, self.root_reason);
    }

    pub(super) fn opaque_root_reason(&mut self, cell: usize, reason: AsciiLiteral) {
        // SAFETY: has just been marked.
        self.sink
            .opaque_root_reason(unsafe { Gc::from_address(cell) }, reason);
    }
}

#[repr(C)]
pub struct HeapObserver {
    pub will_collect: unsafe extern "C" fn(&HeapObserver),
    pub did_collect: unsafe extern "C" fn(&HeapObserver, Scope),
}

impl HeapObserver {
    pub(super) fn will_collect(&self) {
        // SAFETY: promised by whoever added it.
        unsafe { (self.will_collect)(self) }
    }

    pub(super) fn did_collect(&self, scope: Scope) {
        // SAFETY: as above.
        unsafe { (self.did_collect)(self, scope) }
    }
}

/// Roots that only the embedder knows.
pub struct MarkingConstraint {
    execute: unsafe extern "C" fn(*mut c_void, &mut Visitor),
    destroy: unsafe extern "C" fn(*mut c_void),
    context: *mut c_void,
    pub(super) depends_on_marking: bool,
}

impl MarkingConstraint {
    /// # Safety
    /// The functions can be called with `context`, `destroy` last.
    pub unsafe fn new(
        execute: unsafe extern "C" fn(*mut c_void, &mut Visitor),
        destroy: unsafe extern "C" fn(*mut c_void),
        context: *mut c_void,
        depends_on_marking: bool,
    ) -> MarkingConstraint {
        MarkingConstraint {
            execute,
            destroy,
            context,
            depends_on_marking,
        }
    }

    pub(super) fn execute(&self, visitor: &mut Visitor) {
        // SAFETY: promised by its maker.
        unsafe { (self.execute)(self.context, visitor) }
    }
}

impl Drop for MarkingConstraint {
    fn drop(&mut self) {
        // SAFETY: as above.
        unsafe { (self.destroy)(self.context) }
    }
}
