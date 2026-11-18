use bun_engine_text::PropertyName;
use bun_engine_vm::code::metadata::{GetGlobal, GetImport, GlobalAccess};
use bun_engine_vm::{
    Cx, Environment, GlobalBinding, GlobalPlace, JsCell, ModuleEnvironment, ShapeId, scope,
};

use super::{Frame, execute};

fn cache_binding(access: &GlobalAccess, binding: &GlobalBinding) {
    access.shape.set(ShapeId::NONE);
    access
        .pointer
        .set(core::ptr::from_ref(binding).expose_provenance());
}

fn cache_place(cx: &Cx, f: Frame<'_>, access: &GlobalAccess, place: GlobalPlace<'_>, stores: bool) {
    match place {
        GlobalPlace::Lexical(binding) | GlobalPlace::Variable(binding) => {
            cache_binding(access, binding);
        }
        GlobalPlace::Property(shape, offset) => {
            if stores {
                shape.did_cache_property_replacement(cx, offset);
            }
            access.shape.set(shape.id());
            access.byte_offset.set(offset.byte_offset() as i32);
            access.pointer.set(cx.global().address());
            f.code_block.write_barrier_for_cell();
        }
        GlobalPlace::Unknown => {}
    }
}

fn cache_store(cx: &Cx, f: Frame<'_>, name: PropertyName<'_>, metadata: u32) {
    let global = cx.global();
    cache_place(cx, f, f.entry(metadata), global.place_for_store(name), true);
}

execute! {
    CreateScope(op, cx, f) {
        let template = f.code_block.scope_template(cx, op.scope);
        // SAFETY: what the generator has arranged.
        f.set(op.dst, unsafe { Environment::create(cx, &cx.global(), f.cell(op.parent), template) });
    }
    CopyScope(op, cx, f) {
        f.set(op.scope, f.cell::<Environment>(op.scope).copy(cx));
    }
    CreateWithScope(op, cx, f) {
        let object = f.get(op.object).to_object(cx)?;
        let template = f.code_block.scope_template(cx, op.scope);
        // SAFETY: as above.
        f.set(op.dst, unsafe { Environment::create_with(cx, f.cell(op.parent), template, object) });
    }
    GetScoped(op, cx, f) {
        let value = f.cell::<Environment>(op.scope).get(op.slot).expect("initialized");
        f.set_profiled(op.dst, op.metadata, value);
    }
    GetOuter(op, cx, f) {
        let value = f.outer_scope(op.scope, op.depth).get(op.slot).expect("initialized");
        f.set_profiled(op.dst, op.metadata, value);
    }
    GetScopedChecked(op, cx, f) {
        let value = f.cell::<Environment>(op.scope).get_checked(cx, op.slot)?;
        f.set_profiled(op.dst, op.metadata, value);
    }
    GetOuterChecked(op, cx, f) {
        let value = f.outer_scope(op.scope, op.depth).get_checked(cx, op.slot)?;
        f.set_profiled(op.dst, op.metadata, value);
    }
    GetInternal(op, cx, f) {
        f.set_or_empty(op.dst, f.outer_scope(op.scope, op.depth).get(op.slot));
    }
    PutScoped(op, cx, f) {
        // SAFETY: as above.
        unsafe { f.cell::<Environment>(op.scope).set(op.slot, f.get_or_empty(op.value)) };
    }
    PutOuter(op, cx, f) {
        // SAFETY: as above.
        unsafe { f.outer_scope(op.scope, op.depth).set(op.slot, f.get_or_empty(op.value)) };
    }
    GetImport(op, cx, f) {
        let module = f.outer_scope(op.scope, op.depth).cast::<ModuleEnvironment>();
        let value = module.get_import(cx, op.slot)?;
        let entry = f.entry::<GetImport>(op.metadata);
        if let Some((environment, slot)) = module.resolved_import(op.slot) {
            // SAFETY: what the linker has resolved the import to.
            let address = core::ptr::from_ref(unsafe { environment.slot_unchecked(slot) });
            entry.slot.set(address.expose_provenance());
            entry.environment.set(environment.address());
        }
        f.set_profiled(op.dst, op.metadata, value);
    }
    GetGlobal(op, cx, f) {
        let name = f.name(op.name);
        let value = cx.get_global(name, op.unresolved)?;
        let (global, access) = (cx.global(), &f.entry::<GetGlobal>(op.metadata).access);
        cache_place(cx, f, access, global.place_for_load(name), false);
        f.set_profiled(op.dst, op.metadata, value);
    }
    PutGlobal(op, cx, f) {
        cx.put_global(f.name(op.name), f.get(op.value), op.strict)?;
        cache_store(cx, f, f.name(op.name), op.metadata);
    }
    InitializeGlobal(op, cx, f) {
        cx.initialize_global(f.name(op.name), f.get(op.value));
    }
    DeleteGlobal(op, cx, f) {
        f.set(op.dst, cx.delete_global(f.name(op.name))?);
    }
    ResolveDynamic(op, cx, f) {
        f.set(op.dst, cx.resolve_dynamic(f.cell(op.scope), f.name(op.name))?);
    }
    GetDynamic(op, cx, f) {
        let value = cx.get_dynamic(f.cell(op.holder), f.name(op.name), op.unresolved, op.strict)?;
        f.set_profiled(op.dst, op.metadata, value);
    }
    PutDynamic(op, cx, f) {
        cx.put_dynamic(f.cell(op.holder), f.name(op.name), f.get(op.value), op.strict)?;
    }
    DeleteDynamic(op, cx, f) {
        f.set(op.dst, cx.delete_dynamic(f.cell(op.holder), f.name(op.name))?);
    }
    GetDynamicThis(op, cx, f) {
        f.set(op.dst, scope::dynamic_this(f.cell::<JsCell>(op.holder)));
    }
    PutHoisted(op, cx, f) {
        cx.put_hoisted(f.cell(op.scope), f.name(op.name), f.reg(op.value))?;
    }
    DirectEval(op, cx, f) {
        let (source, scope) = (f.get(op.source), f.cell(op.scope));
        let result = bun_engine_vm::direct_eval(cx, f.code_block, source, scope, op.context)?;
        f.set_profiled(op.dst, op.metadata, result);
    }
    DynamicImport(op, cx, f) {
        let (specifier, options) = (f.get(op.specifier), f.get(op.options));
        let scope = f.cell(op.scope);
        f.set(op.dst, crate::pending::dynamic_import(cx, f.code_block, specifier, options, scope, op.phase)?);
    }
}
