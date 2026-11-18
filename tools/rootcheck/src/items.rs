//! Statics and the fields of types.

use rustc_data_structures::fx::FxHashSet;
use rustc_hir::def::DefKind;
use rustc_hir::def_id::{DefId, LOCAL_CRATE};
use rustc_middle::ty::{self, ImplPolarity, Ty, TypingEnv};

use crate::engine::init_of;
use crate::types::Types;

pub fn check(cx: &Types<'_>) {
    let tcx = cx.tcx;
    let inits = check_inits(cx);
    let of_kind = |kind: fn(DefKind) -> bool| {
        let definitions = tcx.hir_crate_items(()).definitions().map(|it| it.to_def_id());
        definitions.filter(move |&it| kind(tcx.def_kind(it)))
    };
    let types = || of_kind(|it| matches!(it, DefKind::Struct | DefKind::Enum | DefKind::Union));
    let mut traced = FxHashSet::default();
    for def_id in types().filter(|&it| is_visited(cx, it)) {
        add_with_content(cx, def_id, &mut traced);
    }
    traced.retain(|it| !inits.contains(it));
    cx.traced.set(traced).expect("set once");
    types().for_each(|it| check_fields(cx, it, inits.contains(&it)));
    of_kind(|it| matches!(it, DefKind::Static { .. })).for_each(|it| check_static(cx, it));
}

fn type_of<'tcx>(cx: &Types<'tcx>, def_id: DefId) -> (TypingEnv<'tcx>, Ty<'tcx>) {
    let env = TypingEnv::post_analysis(cx.tcx, def_id);
    (env, cx.normalize(env, cx.tcx.type_of(def_id).instantiate_identity()))
}

fn is_visited(cx: &Types<'_>, def_id: DefId) -> bool {
    let (ty, engine) = (type_of(cx, def_id).1, &cx.engine);
    [engine.trace, engine.cell_kind, engine.service].iter().any(|&it| cx.implements(ty, it))
        || cx.declared_polarity(ty) == Some(ImplPolarity::Positive)
        || cx.allows.has(cx.tcx, def_id)
}

fn add_with_content(cx: &Types<'_>, def_id: DefId, traced: &mut FxHashSet<DefId>) {
    if !def_id.is_local() || !traced.insert(def_id) {
        return;
    }
    for field in cx.tcx.adt_def(def_id).all_fields() {
        let mut walker = type_of(cx, field.did).1.walk();
        while let Some(argument) = walker.next() {
            match argument.as_type().map(|it| it.kind()) {
                Some(ty::Adt(def, _)) => add_with_content(cx, def.did(), traced),
                Some(ty::Ref(..) | ty::FnPtr(..) | ty::Dynamic(..)) => {
                    walker.skip_current_subtree()
                }
                _ => {}
            }
        }
    }
}

const INIT_NOTE: &str = "`Vm::allocate` may collect while it holds what the cell is made from";
const INIT_HELP: &str =
    "use `TracedVec`, `TracedMap` or `GcCell`, which start empty, and fill it afterwards";

/// Returns those that are structs of this crate, which are looked at field by field.
fn check_inits(cx: &Types<'_>) -> FxHashSet<DefId> {
    let tcx = cx.tcx;
    let mut structs = FxHashSet::default();
    for &impl_id in tcx.trait_impls_in_crate(LOCAL_CRATE) {
        if tcx.impl_trait_id(impl_id) != cx.engine.cell_kind {
            continue;
        }
        let init = init_of(tcx, impl_id);
        let (env, ty) = type_of(cx, init);
        if let Some(def) = ty.ty_adt_def().filter(|it| it.did().is_local()) {
            structs.insert(def.did());
        } else if let Some(hidden) = cx.hides(env, ty) {
            cx.report("hidden", tcx.def_span(init), hidden.message())
                .with_note(INIT_NOTE)
                .with_help(INIT_HELP)
                .emit();
        }
    }
    structs
}

fn check_fields(cx: &Types<'_>, def_id: DefId, is_init: bool) {
    let tcx = cx.tcx;
    let def = tcx.adt_def(def_id);
    for field in def.all_fields() {
        let Some(hidden) = cx.hidden_by_field(def, field.did) else {
            continue;
        };
        if cx.allows.is_on(tcx, field.did) {
            continue;
        }
        let span = tcx.def_ident_span(field.did).unwrap_or_else(|| tcx.def_span(field.did));
        let diagnostic = cx.report("hidden", span.source_callsite(), hidden.message());
        if is_init {
            diagnostic.with_note(INIT_NOTE).with_help(INIT_HELP).emit();
        } else {
            let name = tcx.item_name(def_id);
            diagnostic
                .with_note(format!("`{name}` is neither traced by a cell nor visited by the `Vm`"))
                .with_help(crate::bodies::HELP)
                .emit();
        }
    }
}

fn check_static(cx: &Types<'_>, def_id: DefId) {
    let (env, ty) = type_of(cx, def_id);
    if cx.refers(env, ty) && !cx.allows.is_on(cx.tcx, def_id) {
        let span = cx.tcx.def_span(def_id).source_callsite();
        cx.report("static", span, "the collector does not scan statics and thread-locals".into())
            .with_help("keep it in the `State` of a `Service`, or in a `Strong`")
            .emit();
    }
}
