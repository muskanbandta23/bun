//! What all threads share: the host, the options and the stubs.

use bun_engine_asm::Features as CpuFeatures;
use bun_engine_exec_memory::OutOfExecutableMemory;
use bun_engine_wasm_compiler::abi::{Callable, HostLayout};
use bun_engine_wasm_format::Features;

use crate::code::Optimizer;
use crate::faults;
use crate::host::Host;
use crate::stubs::StubCode;
use crate::value::HostCell;

#[derive(Clone, Copy, Debug)]
pub enum Tiering {
    BaselineOnly,
    /// Functions are optimized when they are first called.
    OptimizedOnly,
    /// After this many turns of loops, or what corresponds to them in calls.
    After(u32),
}

impl Tiering {
    pub const DEFAULT: Self = Self::After(50_000);
}

#[derive(Clone, Copy)]
pub struct Options {
    pub features: Features,
    pub cpu: CpuFeatures,
    /// Without it all checks are explicit.
    pub handle_faults: bool,
    /// Takes `handle_faults`.
    pub guard_memories: bool,
    pub tiering: Tiering,
    pub optimizer: Optimizer,
    /// Without it all work is done by the thread that asks for it, at once.
    pub use_threads: bool,
    /// Random bits.
    pub seed: u64,
}

pub struct Engine {
    host: &'static dyn Host,
    layout: HostLayout,
    options: Options,
    stubs: StubCode,
}

impl Engine {
    /// It stays for good. Code of different engines does not meet on a stack.
    pub fn new(
        host: &'static dyn Host,
        mut options: Options,
    ) -> Result<&'static Self, OutOfExecutableMemory> {
        options.handle_faults = options.handle_faults && faults::install_handler();
        options.guard_memories &= options.handle_faults;
        let mut layout = host.layout();
        layout.null_guard_size = match options.handle_faults {
            true => faults::NULL_GUARD_SIZE,
            false => 0,
        };
        Ok(Box::leak(Box::new(Self {
            host,
            layout,
            options,
            stubs: StubCode::new(&layout, options.cpu)?,
        })))
    }

    #[inline]
    pub(crate) fn host(&self) -> &'static dyn Host {
        self.host
    }

    #[inline]
    pub fn layout(&self) -> &HostLayout {
        &self.layout
    }

    /// With what turned out to be impossible switched off.
    #[inline]
    pub fn options(&self) -> &Options {
        &self.options
    }

    #[inline]
    pub(crate) fn stubs(&self) -> &StubCode {
        &self.stubs
    }

    /// The stub for an import of any type that leads to [`Host::call`].
    pub fn host_call_stub(&self) -> usize {
        self.stubs.host_call
    }

    /// Calls a function of any type. It is called like a JavaScript function, with a `funcref` for the callee, and
    /// leaves the rest to [`Host::put_arguments`] and [`Host::take_results`].
    pub fn enter_stub(&self) -> usize {
        self.stubs.enter
    }

    /// # Safety
    /// `function` is a `funcref`.
    #[inline]
    pub unsafe fn callable_of(&self, function: HostCell) -> Callable {
        // SAFETY: the caller's.
        unsafe {
            function
                .field::<Callable>(self.layout.function_callable)
                .read()
        }
    }
}
