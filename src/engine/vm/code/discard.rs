//! Gives up code that can be made again.

use std::rc::Rc;

use bun_engine_bytecode::Specialization;

use crate::heap::{RootMap, Scope};
use crate::{
    CodeBlock, Coroutine, Cx, FastHashSet, FunctionExecutable, Gc, RootVec, ScriptExecutable, Vm,
    call, root_vec,
};

const SPECIALIZATIONS: [Specialization; 2] = [Specialization::Call, Specialization::Construct];

impl Vm {
    fn script_executables(&self) -> RootVec<Gc<ScriptExecutable>> {
        let mut executables = root_vec();
        (self.heap).for_each_live_cell(|cell| executables.extend(cell.downcast()));
        executables
    }

    /// `visit` does not allocate.
    pub fn for_each_code_block(&self, visit: impl FnMut(Gc<CodeBlock>)) {
        self.heap.for_each_live(visit);
    }

    /// No script is running. What is suspended goes on in the code that it has.
    fn discard_linked_code(&self) {
        let (mut executables, mut coroutines) = (root_vec(), root_vec());
        self.heap.for_each_live_cell(|cell| {
            executables.extend(cell.downcast::<ScriptExecutable>());
            coroutines.extend(cell.downcast::<Coroutine>().filter(|it| it.is_suspended()));
        });
        let mut copies = RootMap::default();
        for coroutine in coroutines {
            if let Some(function) = coroutine.executable().downcast::<FunctionExecutable>() {
                let copy = copies.entry(function.address());
                coroutine.set_executable(*copy.or_insert_with(|| self.copy_with_code(&function)));
            }
        }
        for executable in executables {
            for specialization in SPECIALIZATIONS {
                if let Some(code_block) = executable.code_block(specialization) {
                    code_block.jettison(self);
                    executable.clear_code(specialization);
                }
            }
            executable.forget_evals();
        }
    }

    /// No script is running.
    fn copy_with_code(&self, function: &FunctionExecutable) -> Gc<ScriptExecutable> {
        let code_block = function.code_block(Specialization::Call);
        let code_block = code_block.expect("what a coroutine is suspended in");
        let (provider, info) = (function.provider().clone(), function.info().clone());
        let copy = FunctionExecutable::create(self, provider, Some(function.top_level()), info);
        // SAFETY: the caller's.
        let cx = unsafe { Cx::from_global(code_block.global()) };
        let code = Rc::clone(code_block.code());
        if call::install_code(cx, copy.upcast(), Specialization::Call, code).is_err() {
            bun_alloc::out_of_memory();
        }
        copy.upcast()
    }

    /// The bytecode of functions that no code block is linked from.
    pub fn discard_unlinked_code(&self) {
        let mut pending = self.cached_function_tables();
        pending.extend(self.builtin_function_tables());
        for executable in self.script_executables() {
            if let Some(function) = executable.downcast::<FunctionExecutable>() {
                pending.push(Rc::clone(function.info().table()));
            }
        }
        let mut seen = FastHashSet::default();
        while let Some(table) = pending.pop() {
            if !seen.insert(table.as_ptr().addr()) {
                continue;
            }
            for function in table.iter() {
                for specialization in SPECIALIZATIONS {
                    let Some(code) = function.code(specialization) else {
                        continue;
                    };
                    // This reference, and that of the function.
                    if Rc::strong_count(&code) == 2 {
                        function.set_code(specialization, None);
                    } else {
                        pending.push(Rc::clone(&code.functions));
                    }
                }
            }
        }
    }

    /// As soon as no script is running.
    pub fn discard_all_code(&self) {
        self.when_idle(|vm| {
            vm.clear_code_cache();
            vm.delete_regexp_code();
            vm.discard_linked_code();
            for executable in vm.script_executables() {
                if let Some(function) = executable.downcast::<FunctionExecutable>() {
                    for specialization in SPECIALIZATIONS {
                        function.info().set_code(specialization, None);
                    }
                }
            }
        });
    }

    /// As soon as no script is running.
    pub fn shrink_footprint(&self) {
        self.when_idle(|vm| {
            vm.clear_last_exception();
            vm.discard_all_code();
            vm.clear_source_caches();
            vm.heap.collect_and_sweep(Some(Scope::Full));
        });
    }
}
