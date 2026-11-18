use core::cell::Cell;
use core::mem::offset_of;

use crate::code::metadata::{Entry, WeakCell};
use crate::heap::Survivors;
use crate::{CellType, CodeBlock, Gc, JsCell, JsFunction, Value, Visitor};

/// What a call site has called.
#[repr(C)]
pub struct CallProfile {
    /// The only callee so far, which has an executable. `MANY` if there have been others.
    callee: WeakCell,
    /// The only executable so far, for a site that calls several closures of it.
    executable: WeakCell,
    /// Null, or `MAX_TARGETS` entries that the code block owns.
    targets: Cell<*const CallTargetCount>,
}

/// An executable or, where that does not say enough, a callee.
#[repr(C)]
pub struct CallTargetCount {
    pub target: WeakCell,
    pub count: Cell<u32>,
}

impl CallProfile {
    pub const OFFSET_OF_CALLEE: usize = offset_of!(CallProfile, callee);
    pub const OFFSET_OF_EXECUTABLE: usize = offset_of!(CallProfile, executable);
    pub const OFFSET_OF_TARGETS: usize = offset_of!(CallProfile, targets);
    pub const MAX_TARGETS: usize = 6;
    /// In `executable`: see `targets`.
    pub const POLYMORPHIC: usize = WeakCell::MANY;
    /// In `executable`: nothing is recorded any more.
    pub const MEGAMORPHIC: usize = 2;

    #[inline(always)]
    pub fn callee(&self) -> &WeakCell {
        &self.callee
    }

    #[inline(always)]
    pub fn executable(&self) -> &WeakCell {
        &self.executable
    }

    pub fn targets(&self) -> &[CallTargetCount] {
        if self.targets.get().is_null() {
            return &[];
        }
        // SAFETY: from `allocate_call_targets` of the code block that has this metadata.
        unsafe { core::slice::from_raw_parts(self.targets.get(), Self::MAX_TARGETS) }
    }

    pub fn has_run(&self) -> bool {
        self.callee.bits() != WeakCell::NONE
    }

    fn target_of(callee: Gc<JsCell>) -> Gc<JsCell> {
        match callee.downcast::<JsFunction>() {
            Some(function) if !function.is_native() => function.executable().cell(),
            _ => callee,
        }
    }

    /// If the site is polymorphic and knows the target. `executable` is what the callee has in that field.
    #[inline(always)]
    pub fn try_count(&self, callee: usize, executable: usize) -> bool {
        let known = |it: &&CallTargetCount| it.target.bits() == executable || it.target.bits() == callee;
        match self.targets().iter().find(known) {
            Some(entry) => entry.count.update(|count| count.wrapping_add(1)),
            None => return false,
        }
        true
    }

    /// For a call that matches neither the callee nor the executable.
    #[inline(never)]
    pub fn record(&self, owner: &CodeBlock, callee: Value) {
        let state = self.executable.bits();
        let callee = callee
            .as_cell()
            .filter(|cell| CellType::FUNCTIONS.contains(cell.cell_type()));
        let (Some(callee), true) = (callee, state != Self::MEGAMORPHIC) else {
            self.callee.set_special(WeakCell::MANY);
            return self.executable.set_special(Self::MEGAMORPHIC);
        };
        let target = Self::target_of(callee);
        let first = self.callee.get();
        if state == WeakCell::NONE && first.is_none() {
            return self.callee.set(owner, &callee);
        }
        self.callee.set_special(WeakCell::MANY);
        let earlier = first.map(Self::target_of).or_else(|| self.executable.get());
        if earlier == Some(target) && target != callee {
            return self.executable.set(owner, &target);
        }
        if self.targets.get().is_null() {
            self.targets.set(owner.allocate_call_targets());
        }
        self.executable.set_special(Self::POLYMORPHIC);
        for target in earlier.into_iter().chain([target]) {
            let has = |bits| self.targets().iter().find(|it| it.target.bits() == bits);
            let Some(entry) = has(target.address()).or_else(|| has(WeakCell::NONE)) else {
                return self.executable.set_special(Self::MEGAMORPHIC);
            };
            entry.target.set(owner, &target);
            entry.count.update(|count| count.wrapping_add(1));
        }
    }
}

// SAFETY: integers and a pointer.
unsafe impl Entry for CallProfile {
    fn visit(&self, _: &mut Visitor) {}

    fn reconcile(&self, _: &CodeBlock, survivors: &Survivors) {
        // The closure is gone, but the next one is likely to be of the same code.
        if let Some(callee) = self.callee.get().filter(|&it| !survivors.contains(it)) {
            let target = Self::target_of(callee);
            self.callee.set_special(WeakCell::MANY);
            if target != callee {
                self.executable.set_unbarriered(&target);
            }
        }
        self.executable.clear_if_dead(survivors);
        for entry in self.targets() {
            if entry.target.clear_if_dead(survivors) {
                entry.count.set(0);
            }
        }
    }
}
