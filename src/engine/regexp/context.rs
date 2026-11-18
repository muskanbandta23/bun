//! The memory that matching works in, and how it ends.

use core::mem::offset_of;

use bun_engine_unicode::IgnoreCase;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Match {
        start: u32,
        end: u32,
    },
    NoMatch,
    /// The function given to [`Context::set_poll`] said so.
    Interrupted,
    OutOfMemory,
}

/// The work between two polls.
pub const FUEL: u32 = 1 << 18;

const INITIAL_WORDS: usize = 1 << 10;
const KEPT_WORDS: usize = 1 << 16;
const DEFAULT_MAX_STACK_BYTES: usize = 192 << 20;

/// Words that are all initialized, and that generated code finds by address.
#[repr(C)]
pub(crate) struct Words {
    start: usize,
    end: usize,
    words: Vec<u32>,
}

impl Words {
    const fn new() -> Words {
        Words {
            start: 0,
            end: 0,
            words: Vec::new(),
        }
    }

    #[inline]
    pub(crate) fn as_mut_slice(&mut self) -> &mut [u32] {
        &mut self.words
    }

    /// Keeps the contents.
    #[inline]
    pub(crate) fn ensure(&mut self, length: usize, max: usize) -> Option<()> {
        if length > self.words.len() {
            self.grow(length, max)?;
        }
        Some(())
    }

    #[cold]
    fn grow(&mut self, length: usize, max: usize) -> Option<()> {
        if length > max {
            return None;
        }
        let doubled = (self.words.len() * 2).max(INITIAL_WORDS);
        let length = doubled.min(max).max(length);
        let additional = length - self.words.len();
        self.words.try_reserve_exact(additional).ok()?;
        self.words.resize(length, 0);
        self.publish();
        Some(())
    }

    fn trim(&mut self) {
        if self.words.len() > KEPT_WORDS {
            self.words = Vec::new();
            self.publish();
        }
    }

    fn publish(&mut self) {
        let range = self.words.as_mut_ptr_range();
        self.start = range.start.expose_provenance();
        self.end = range.end.expose_provenance();
    }
}

/// One is enough for a thread: matching runs no script.
#[repr(C)]
pub struct Context {
    pub(crate) stack: Words,
    pub(crate) cells: Words,
    pub(crate) meter: Meter,
    pub(crate) max_stack_words: usize,
}

#[derive(Default)]
pub(crate) struct Meter {
    pub(crate) work: u64,
    poll: Option<Box<dyn FnMut() -> bool>>,
}

impl Meter {
    /// Whether to go on.
    #[cold]
    pub(crate) fn poll(&mut self, spent: u64) -> bool {
        self.work = self.work.saturating_add(spent);
        !self.poll.as_mut().is_some_and(|should_stop| should_stop())
    }
}

impl Context {
    pub const OFFSET_OF_STACK_START: usize = offset_of!(Context, stack.start);
    pub const OFFSET_OF_STACK_END: usize = offset_of!(Context, stack.end);
    pub const OFFSET_OF_CELLS_START: usize = offset_of!(Context, cells.start);
    pub const OFFSET_OF_CELLS_END: usize = offset_of!(Context, cells.end);

    pub fn new() -> Context {
        Context {
            stack: Words::new(),
            cells: Words::new(),
            meter: Meter::default(),
            max_stack_words: DEFAULT_MAX_STACK_BYTES / size_of::<u32>(),
        }
    }

    /// `should_stop` is asked now and then during a long match.
    pub fn set_poll(&mut self, should_stop: Box<dyn FnMut() -> bool>) {
        self.meter.poll = Some(should_stop);
    }

    pub fn set_max_stack_size(&mut self, bytes: usize) {
        self.max_stack_words = bytes / size_of::<u32>();
        self.stack = Words::new();
    }

    /// About the number of characters that the interpreter has looked at since the last call.
    #[inline]
    pub fn take_work(&mut self) -> u64 {
        core::mem::take(&mut self.meter.work)
    }

    /// Frees what an unusually deep match has left.
    pub fn trim(&mut self) {
        self.stack.trim();
        self.cells.trim();
    }
}

macro_rules! helpers {
    ($($(#[$meta:meta])* fn $name:ident $arguments:tt -> $result:ty $body:block)*) => {$(
        #[cfg(target_arch = "x86_64")]
        $(#[$meta])*
        pub extern "sysv64" fn $name $arguments -> $result $body

        #[cfg(not(target_arch = "x86_64"))]
        $(#[$meta])*
        pub extern "C" fn $name $arguments -> $result $body
    )*};
}

/// What generated code calls.
pub mod helper {
    use super::{Context, FUEL, IgnoreCase};

    helpers! {
        /// Makes room for `words` words at `top`, and returns where `top` is now. Zero if there is no memory.
        fn grow_stack(context: &mut Context, top: usize, words: usize) -> usize {
            let used = (top - context.stack.start) / size_of::<u32>();
            match context.stack.ensure(used + words, context.max_stack_words) {
                Some(()) => context.stack.start + used * size_of::<u32>(),
                None => 0,
            }
        }

        /// Returns the address of `words` words. Zero if there is no memory.
        fn grow_cells(context: &mut Context, words: usize) -> usize {
            match context.cells.ensure(words, usize::MAX) {
                Some(()) => context.cells.start,
                None => 0,
            }
        }

        /// Returns more fuel, or zero to stop.
        fn poll(context: &mut Context) -> usize {
            match context.meter.poll(FUEL.into()) {
                true => FUEL as usize,
                false => 0,
            }
        }

        /// Not zero if the characters are equal when case is ignored.
        fn equal_ignoring_case(a: u32, b: u32, unicode_aware: u32) -> usize {
            let equivalence = match unicode_aware {
                0 => IgnoreCase::CodeUnits,
                _ => IgnoreCase::CodePoints,
            };
            usize::from(equivalence.canonicalize(a) == equivalence.canonicalize(b))
        }
    }
}
