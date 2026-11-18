//! Throwing code away, to save memory or to generate it in another way.

use std::rc::Rc;

use bun_engine_bytecode::Specialization::{Call, Construct};

use crate::heap::{RootAlloc, RootMap};
use crate::{
    CodeBlock, Coroutine, Cx, FunctionExecutable, Gc, ScriptExecutable, Vm, call, root_vec,
};

impl Vm {
    /// `visit` does not allocate.
    pub fn for_each_code_block(&self, visit: impl FnMut(Gc<CodeBlock>)) {
        self.heap.for_each_live(visit);
    }

    pub fn jettison_all_optimized_code(&self) {
        let mut optimized = root_vec();
        self.for_each_code_block(|it| optimized.extend(it.is_optimized().then_some(it)));
        optimized.iter().for_each(|it| it.jettison(self));
    }

    /// Once no script is running. What is suspended goes on in the code that it has.
    pub fn delete_all_code(&self) {
        self.when_idle(Vm::delete_all_code_now);
    }

    fn delete_all_code_now(&self) {
        self.jettison_all_optimized_code();
        self.clear_code_cache();
        let (mut executables, mut coroutines) = (root_vec(), root_vec());
        self.heap.for_each_live_cell(|cell| {
            executables.extend(cell.downcast::<ScriptExecutable>());
            coroutines.extend(cell.downcast::<Coroutine>().filter(|it| it.is_suspended()));
        });
        let mut successors = RootMap::with_hasher_in(Default::default(), RootAlloc::default());
        for coroutine in coroutines {
            if let Some(function) = coroutine.executable().downcast::<FunctionExecutable>() {
                let successor = successors.entry(function.address());
                coroutine.set_executable(*successor.or_insert_with(|| self.copy_of(&function)));
            }
        }
        for executable in executables {
            executable.forget_evals();
            for specialization in [Call, Construct] {
                executable.clear_code(specialization);
                if let Some(function) = executable.downcast::<FunctionExecutable>() {
                    function.info().set_code(specialization, None);
                }
            }
        }
    }

    /// With the code that `function` has now. No script is running.
    fn copy_of(&self, function: &FunctionExecutable) -> Gc<ScriptExecutable> {
        let code_block = (function.code_block(Call)).expect("what a coroutine is suspended in");
        let (provider, info) = (function.provider().clone(), function.info().clone());
        let copy = FunctionExecutable::create(self, provider, Some(function.top_level()), info);
        // SAFETY: the caller's.
        let cx = unsafe { Cx::from_global(code_block.global()) };
        if call::install_code(cx, copy.upcast(), Call, Rc::clone(code_block.code())).is_err() {
            bun_alloc::out_of_memory();
        }
        copy.upcast()
    }
}
