//! Words in instances that mirror the state of something else.

use core::cell::RefCell;

pub(crate) struct Registry<T> {
    state: RefCell<State<T>>,
}

struct State<T> {
    /// Null where there is room.
    entries: Vec<*mut T>,
    free: Vec<u32>,
}

#[derive(Clone, Copy)]
pub(crate) struct Registration(u32);

impl<T> Default for Registry<T> {
    fn default() -> Self {
        let state = State {
            entries: Vec::new(),
            free: Vec::new(),
        };
        Self {
            state: RefCell::new(state),
        }
    }
}

impl<T> Registry<T> {
    /// # Safety
    /// `entry` is valid for writes on this thread until it is removed.
    pub(crate) unsafe fn add(&self, entry: *mut T) -> Registration {
        let state = &mut *self.state.borrow_mut();
        if let Some(index) = state.free.pop() {
            state.entries[index as usize] = entry;
            return Registration(index);
        }
        state.entries.push(entry);
        Registration(state.entries.len() as u32 - 1)
    }

    pub(crate) fn remove(&self, registration: Registration) {
        let state = &mut *self.state.borrow_mut();
        state.entries[registration.0 as usize] = core::ptr::null_mut();
        state.free.push(registration.0);
        if state.free.len() == state.entries.len() {
            state.free.clear();
            state.entries.clear();
        }
    }

    pub(crate) fn write_all(&self, value: impl Fn() -> T) {
        for &entry in &self.state.borrow().entries {
            if !entry.is_null() {
                // SAFETY: it was added and not removed.
                unsafe { entry.write(value()) };
            }
        }
    }
}
