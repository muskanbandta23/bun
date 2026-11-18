//! The global object of the shell.

use bun_engine_macros::{cell, object_ops};
use bun_engine_text::symbols;
use bun_engine_vm::{
    DontEnumMode, GlobalObject, Inherited, ModuleHooks, PropertyNameList, RealmHooks,
    ScriptExecutionStatus, Shape, ShapeSpec, Vm,
};

use crate::prelude::*;
use crate::{functions, run};

#[cell(name = "global", ops)]
struct ShellGlobal {
    base: GlobalObject,
}

#[object_ops]
impl ShellGlobal {
    fn own_property_names(
        &self,
        cx: &mut Cx,
        list: &mut PropertyNameList,
        mode: DontEnumMode,
    ) -> JsResult<()> {
        let mut all = PropertyNameList::new(list.mode());
        self.base_own_property_names(cx, &mut all, mode)?;
        let state = State::of(cx);
        let hidden = state.hidden_globals.borrow();
        for name in all.iter().filter(|name| !hidden.contains(*name)) {
            list.add(name.name());
        }
        Ok(())
    }
}

static HOOKS: RealmHooks = RealmHooks {
    modules: ModuleHooks {},
    promise_rejection_tracker: |_, _, _| {},
    report_uncaught_exception: |cx, exception| {
        run::dump_exception(cx, exception.value());
        run::exit(run::EXIT_EXCEPTION)
    },
    current_script_execution_owner: |cx| cx.global().upcast(),
    script_execution_status: |_, _| ScriptExecutionStatus::Running,
    default_language: StringRef::empty,
    derive_shadow_realm: |cx| Ok(GlobalObject::create(cx, &HOOKS)),
    compile_wasm_streaming: None,
    instantiate_wasm_streaming: None,
};

pub(crate) fn create(vm: &Vm) -> Gc<GlobalObject> {
    let shape = Shape::create(vm, None, Value::NULL, ShapeSpec::of::<ShellGlobal>());
    let base = GlobalObject::init(vm, shape, &HOOKS, core::ptr::null());
    let global: Gc<GlobalObject> = (vm.allocate::<ShellGlobal>(ShellGlobalInit { base })).upcast();
    global.finish_creation(vm);
    // SAFETY: a shared borrow, as that of the `Vm` is.
    let cx: &Cx = unsafe { Cx::from_global(global) };
    let tag = JsString::from_literal(vm, "global").into();
    let attributes = Attributes::DONT_ENUM | Attributes::READ_ONLY;
    global.put_direct(vm, symbols::toStringTag, tag, attributes);
    let mut hidden = State::of(vm).hidden_globals.take();
    hidden.insert(symbols::toStringTag.to_identifier());
    State::of(vm).hidden_globals.replace(hidden);
    functions::define_globals(cx);
    global
}

pub(crate) fn define_hidden(cx: &Cx, name: &'static str, value: Value) {
    let name = Identifier::from_literal(name);
    (cx.global()).put_direct(cx, name.name(), value, Attributes::DONT_ENUM);
    State::of(cx).hidden_globals.borrow_mut().insert(name);
}
