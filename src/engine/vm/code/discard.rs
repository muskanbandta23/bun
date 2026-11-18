//! Gives up code that can be made again.

use std::rc::Rc;

use bun_engine_bytecode::{CodeFlags, Specialization};

use crate::heap::Scope;
use crate::{FastHashSet, FunctionExecutable, Gc, RootVec, ScriptExecutable, Vm, root_vec};

const SPECIALIZATIONS: [Specialization; 2] = [Specialization::Call, Specialization::Construct];

impl Vm {
    fn script_executables(&self) -> RootVec<Gc<ScriptExecutable>> {
        let mut executables = root_vec();
        (self.heap).for_each_live_cell(|cell| executables.extend(cell.downcast()));
        executables
    }

    /// But for that of generators and their kin, which may be suspended in it.
    fn discard_linked_code(&self) {
        for executable in self.script_executables() {
            for specialization in SPECIALIZATIONS {
                let Some(code_block) = executable.code_block(specialization) else {
                    continue;
                };
                code_block.jettison(self);
                if !code_block.body().flags().contains(CodeFlags::SUSPENDS) {
                    executable.clear_code(specialization);
                }
            }
            executable.forget_evals();
        }
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
