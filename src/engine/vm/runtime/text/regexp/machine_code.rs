use bun_engine_asm::Features;
use bun_engine_exec_memory::{CodeHandle, Priority, link};
use bun_engine_macros::native_abi;
use bun_engine_regexp::{Context, Outcome, Program};
use bun_engine_regexp_jit::{Options, Tables, compile, outcome};
use bun_engine_text::Text;

use crate::{Vm, options};

pub(super) struct MachineCode {
    code: CodeHandle,
    _tables: Tables,
}

#[repr(C)]
struct Results(usize, usize);

#[native_abi]
type Entry = unsafe fn(*const u8, usize, usize, *mut u32, &mut Context) -> Results;

impl MachineCode {
    pub(super) fn is_enabled() -> bool {
        options().use_jit && options().use_regexp_jit
    }

    /// `None` if the program has to stay with the interpreter.
    pub(super) fn new(vm: &Vm, program: &Program) -> Option<MachineCode> {
        let options = Options {
            features: Features::host(),
            seed: vm.random_seed(),
            max_code_size: options().max_regexp_code_size,
        };
        let compiled = compile(program.verify()?, &options).ok()?;
        Some(MachineCode {
            code: link(&compiled.code, Priority::Optional, |_| unreachable!()).ok()?,
            _tables: compiled.tables,
        })
    }

    pub(super) fn entry(&self) -> usize {
        self.code.start()
    }

    pub(super) fn size(&self) -> usize {
        self.code.len()
    }

    pub(super) fn describe(&self, name: impl FnOnce() -> String) {
        self.code.describe(name);
    }
}

/// # Safety
/// `entry` is that of a `MachineCode` that is alive. Its program is for the width of `subject` and reports no more
/// than `captures` has room for.
#[inline(always)]
pub(super) unsafe fn enter(
    entry: usize,
    subject: Text<'_>,
    start: u32,
    captures: &mut [u32],
    context: &mut Context,
) -> Outcome {
    let units = match subject {
        Text::Latin1(characters) => characters.as_ptr(),
        Text::Utf16(units) => units.as_ptr().cast(),
    };
    // SAFETY: the caller's. The code has this type, and the verifier has bounded what it reads.
    let Results(start, end) = unsafe {
        core::mem::transmute::<usize, Entry>(entry)(
            units,
            start as usize,
            subject.len(),
            captures.as_mut_ptr(),
            context,
        )
    };
    outcome(start, end)
}
