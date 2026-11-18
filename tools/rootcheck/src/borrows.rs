//! Borrows of memory that a cell owns outside itself. They do not keep the cell alive.

use rustc_abi::FIRST_VARIANT;
use rustc_borrowck::consumers::{
    BodyWithBorrowckFacts, ConsumerOptions, get_bodies_with_borrowck_facts,
};
use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use rustc_hir::def::DefKind;
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_index::IndexVec;
use rustc_index::bit_set::DenseBitSet;
use rustc_middle::mir::visit::{MutatingUseContext, NonUseContext, PlaceContext, Visitor};
use rustc_middle::mir::{
    BasicBlock, Body, Local, Location, Operand, Place, ProjectionElem, Rvalue, Terminator,
    TerminatorKind,
};
use rustc_middle::ty::{self, Instance, RegionVid, Ty, TypeVisitableExt, TypingEnv};
use rustc_span::{Span, Spanned};

use crate::bodies::{Frame, deferred_blocks};
use crate::calls::{Calls, Instantiations, Way};
use crate::types::Types;

/// To be judged once nothing needs the MIR of the borrow checker any more.
pub struct Suspect<'tcx> {
    def_id: LocalDefId,
    body: &'tcx Body<'tcx>,
    span: Span,
    ty: Ty<'tcx>,
    held_at_end_of: Vec<BasicBlock>,
    escapes: bool,
}

#[derive(Default)]
struct Closures {
    returns_loose_borrow: FxHashSet<DefId>,
}

/// A statement or call that borrows from a cell, and the regions of the references that it does so through.
struct Source<'tcx> {
    location: Location,
    result: Ty<'tcx>,
    through: Vec<Ty<'tcx>>,
    /// Each `&NoGc` that it is given. What is borrowed for no longer than that cannot meet a collection.
    tokens: Vec<Ty<'tcx>>,
}

pub fn find_suspects<'tcx>(cx: &Types<'tcx>) -> Vec<Suspect<'tcx>> {
    let tcx = cx.tcx;
    let (mut suspects, mut closures) = (Vec::new(), Closures::default());
    for root in tcx.hir_body_owners() {
        if tcx.is_typeck_child(root.to_def_id()) || tcx.hir_body_const_context(root).is_some() {
            continue;
        }
        let bodies = || tcx.nested_bodies_within(root).iter().chain([root]);
        let promoted = |def_id| tcx.mir_promoted(def_id).0;
        if bodies().any(|it| promoted(it).is_stolen())
            || bodies().all(|it| sources(cx, &promoted(it).borrow(), &closures).is_empty())
        {
            continue;
        }
        let facts = get_bodies_with_borrowck_facts(tcx, root, ConsumerOptions::PoloniusInputFacts);
        for def_id in bodies() {
            analyze(cx, def_id, &facts[&def_id], &mut closures, &mut suspects);
        }
    }
    suspects
}

fn env_of<'tcx>(cx: &Types<'tcx>, body: &Body<'tcx>) -> TypingEnv<'tcx> {
    TypingEnv::post_analysis(cx.tcx, body.source.def_id())
}

fn erased<'tcx>(cx: &Types<'tcx>, env: TypingEnv<'tcx>, ty: Ty<'tcx>) -> Ty<'tcx> {
    cx.normalize(env, ty::set_aliases_to_non_rigid(cx.tcx, ty))
}

/// Each `&C` in the type, where `C` is a cell.
fn cell_references<'tcx>(
    cx: &Types<'tcx>,
    env: TypingEnv<'tcx>,
    ty: Ty<'tcx>,
) -> impl Iterator<Item = Ty<'tcx>> {
    let types = ty.walk().filter_map(|it| it.as_type());
    types.filter(move |it| matches!(*it.kind(), ty::Ref(_, inner, _) if cx.is_cell(env, inner)))
}

fn sources<'tcx>(cx: &Types<'tcx>, body: &Body<'tcx>, closures: &Closures) -> Vec<Source<'tcx>> {
    let (tcx, env) = (cx.tcx, env_of(cx, body));
    let mut found = Vec::new();
    for (block, data) in body.basic_blocks.iter_enumerated().filter(|(_, it)| !it.is_cleanup) {
        for (statement_index, statement) in data.statements.iter().enumerate() {
            if let Some((_, rvalue @ Rvalue::Ref(_, _, place))) = statement.kind.as_assign()
                && let Some(reference) = lending_reference(cx, env, body, *place)
            {
                let (location, result) =
                    (Location { block, statement_index }, rvalue.ty(body, tcx));
                found.push(Source { location, result, through: vec![reference], tokens: vec![] });
            }
        }
        if let TerminatorKind::Call { func, args, destination, .. } = &data.terminator().kind
            && let result = destination.ty(body, tcx).ty
            && result.has_free_regions() | result.has_erased_regions()
            && let given = args.iter().map(|it| it.node.ty(body, tcx)).collect::<Vec<_>>()
            && let through =
                given.iter().flat_map(|&it| cell_references(cx, env, it)).collect::<Vec<_>>()
            && let lends = given.iter().any(|&it| mentions_lending_closure(it, closures))
            && (lends || !through.is_empty())
            && !loose_regions(cx, env, result, Some(&through)).is_empty()
            && !stays(cx, env, body, func)
        {
            let through = if lends { Vec::new() } else { through };
            let tokens = given.into_iter().filter(|&it| is_no_gc(cx, it)).collect();
            found.push(Source { location: body.terminator_loc(block), result, through, tokens });
        }
    }
    found
}

fn mentions_lending_closure(ty: Ty<'_>, closures: &Closures) -> bool {
    ty.walk().filter_map(|it| it.as_type()).any(|it| match *it.kind() {
        ty::Closure(def_id, _) => closures.returns_loose_borrow.contains(&def_id),
        _ => false,
    })
}

fn stays<'tcx>(
    cx: &Types<'tcx>,
    env: TypingEnv<'tcx>,
    body: &Body<'tcx>,
    function: &Operand<'tcx>,
) -> bool {
    let ty::FnDef(def_id, args) = *erased(cx, env, function.ty(body, cx.tcx)).kind() else {
        return false;
    };
    let resolved = Instance::try_resolve(cx.tcx, env, def_id, args.skip_binder());
    let def_id = resolved.ok().flatten().map_or(def_id, |it| it.def_id());
    cx.allows.stays(cx.tcx, def_id)
}

/// The reference to the cell, if the place is outside the cell or leads there.
fn lending_reference<'tcx>(
    cx: &Types<'tcx>,
    env: TypingEnv<'tcx>,
    body: &Body<'tcx>,
    place: Place<'tcx>,
) -> Option<Ty<'tcx>> {
    let (mut reference, mut is_outside) = (None, false);
    for (base, projection) in place.iter_projections() {
        let base = base.ty(body, cx.tcx).ty;
        match (projection, *base.kind()) {
            (ProjectionElem::Deref, ty::Ref(_, inner, _)) => {
                (reference, is_outside) = (cx.is_cell(env, inner).then_some(base), false);
            }
            (ProjectionElem::Deref, ty::RawPtr(..)) => reference = None,
            (ProjectionElem::Deref, _) => is_outside = true,
            _ => {}
        }
    }
    let ty = erased(cx, env, place.ty(body, cx.tcx).ty);
    reference
        .filter(|_| is_outside || (!cx.is_cell(env, ty) && cx.lends(env, ty)))
        .filter(|_| !is_in_field_that_stays(cx, body, place))
}

fn is_in_field_that_stays<'tcx>(cx: &Types<'tcx>, body: &Body<'tcx>, place: Place<'tcx>) -> bool {
    place.iter_projections().any(|(base, projection)| {
        let base = base.ty(body, cx.tcx);
        let (ProjectionElem::Field(index, _), ty::Adt(def, _)) = (projection, base.ty.kind())
        else {
            return false;
        };
        let variant = def.variant(base.variant_index.unwrap_or(FIRST_VARIANT));
        cx.allows.stays(cx.tcx, variant.fields[index].did)
    })
}

/// The regions of what does not keep a cell alive by itself. If the type is that of what a function makes of
/// `given`, what fits in one of these cells is taken to be there, which is checked where the function is defined.
fn loose_regions<'tcx>(
    cx: &Types<'tcx>,
    env: TypingEnv<'tcx>,
    ty: Ty<'tcx>,
    given: Option<&[Ty<'tcx>]>,
) -> Vec<ty::Region<'tcx>> {
    let mut regions = Vec::new();
    let mut walker = ty.walk();
    while let Some(argument) = walker.next() {
        let Some(ty) = argument.as_type() else { continue };
        let own_regions = match *ty.kind() {
            ty::Ref(region, ..) | ty::Dynamic(_, region) => vec![region],
            ty::Adt(_, args) | ty::Alias(_, ty::AliasTy { args, .. }) => args.regions().collect(),
            ty::FnPtr(..) | ty::FnDef(..) => {
                walker.skip_current_subtree();
                continue;
            }
            _ => continue,
        };
        if !own_regions.is_empty() && is_loose(cx, env, erased(cx, env, ty), given, 0) {
            regions.extend(own_regions);
        }
    }
    regions
}

fn is_loose<'tcx>(
    cx: &Types<'tcx>,
    env: TypingEnv<'tcx>,
    ty: Ty<'tcx>,
    given: Option<&[Ty<'tcx>]>,
    depth: usize,
) -> bool {
    let fits = |part: Ty<'tcx>| {
        let mut cells = given.into_iter().flatten().map(|it| erased(cx, env, it.peel_refs()));
        cells.any(|cell| cx.contains_inline(env, cell, part))
    };
    match *ty.kind() {
        ty::Ref(_, inner, _) => {
            !cx.is_cell(env, inner)
                && !inner.ty_adt_def().is_some_and(|it| cx.engine.outlives_cells(it.did()))
                && (cx.lends(env, inner) || !fits(inner))
        }
        ty::RawPtr(..) | ty::Dynamic(..) => true,
        ty::Adt(def, args) if def.did() == cx.engine.owned => !fits(args.type_at(1)),
        ty::Adt(def, _) if cx.is_guard(def) => false,
        ty::Adt(def, _)
            if given.is_some() && cx.engine.ref_cell_guards.contains(&Some(def.did())) =>
        {
            false
        }
        _ => {
            ty.has_erased_regions()
                && depth < 8
                && cx.constituents(env, ty).into_iter().any(|it| {
                    (it.has_erased_regions() || it.is_raw_ptr())
                        && is_loose(cx, env, it, given, depth + 1)
                })
        }
    }
}

/// `'a: 'b` is an edge from `'a` to `'b`: what is borrowed for the one flows to the other.
#[derive(Default)]
struct Flow {
    edges: FxHashMap<RegionVid, Vec<RegionVid>>,
}

impl Flow {
    fn new(pairs: impl Iterator<Item = (RegionVid, RegionVid)>) -> Flow {
        let mut flow = Flow::default();
        for (from, to) in pairs.collect::<FxHashSet<_>>() {
            flow.edges.entry(from).or_default().push(to);
        }
        flow
    }

    /// Does not go on from where `stops`.
    fn reached_from(
        &self,
        start: impl IntoIterator<Item = RegionVid>,
        stops: impl Fn(RegionVid) -> bool,
    ) -> FxHashSet<RegionVid> {
        let mut reached = FxHashSet::default();
        let mut pending: Vec<_> = start.into_iter().collect();
        while let Some(region) = pending.pop() {
            if reached.insert(region) && !stops(region) {
                pending.extend(self.edges.get(&region).into_iter().flatten());
            }
        }
        reached
    }
}

fn variables<'tcx>(regions: impl IntoIterator<Item = ty::Region<'tcx>>) -> Vec<RegionVid> {
    let variable = |region: ty::Region<'_>| match region.kind() {
        ty::ReVar(variable) => Some(variable),
        _ => None,
    };
    regions.into_iter().filter_map(variable).collect()
}

fn outer_regions<'tcx>(references: impl Iterator<Item = Ty<'tcx>>) -> Vec<RegionVid> {
    references.filter_map(|it| all_variables(it).first().copied()).collect()
}

fn all_variables(ty: Ty<'_>) -> Vec<RegionVid> {
    variables(ty.walk().filter_map(|it| it.as_region()))
}

fn analyze<'tcx>(
    cx: &Types<'tcx>,
    def_id: LocalDefId,
    facts: &BodyWithBorrowckFacts<'tcx>,
    closures: &mut Closures,
    suspects: &mut Vec<Suspect<'tcx>>,
) {
    let (tcx, body) = (cx.tcx, &facts.body);
    let sources = sources(cx, body, closures);
    if sources.is_empty() {
        return;
    }
    let env = env_of(cx, body);
    let (input, table) =
        (facts.input_facts.as_ref().unwrap(), facts.location_table.as_ref().unwrap());
    let constraints = || {
        (input.subset_base.iter()).map(|&(from, to, at)| {
            (RegionVid::from(from), RegionVid::from(to), table.to_location(at))
        })
    };
    let flow_except_at = |cut: &FxHashSet<Location>| {
        Flow::new(constraints().filter(|it| !cut.contains(&it.2)).map(|it| (it.0, it.1)))
    };
    let universal: FxHashSet<RegionVid> =
        input.universal_region.iter().map(|&it| it.into()).collect();

    let (mut anchors, mut releases) = (FxHashSet::default(), FxHashSet::default());
    let mut anchored_at_first = Vec::new();
    let mut anchor = |location, ty| {
        anchors.insert(location);
        anchored_at_first.extend(outer_regions(cell_references(cx, env, ty)));
    };
    for (block, data) in body.basic_blocks.iter_enumerated() {
        for (statement_index, statement) in data.statements.iter().enumerate() {
            if let Some((target, rvalue)) = statement.kind.as_assign()
                && let Rvalue::Ref(_, _, place)
                | Rvalue::Use(Operand::Copy(place) | Operand::Move(place), _) = *rvalue
                && is_in_field_that_stays(cx, body, place)
            {
                anchor(Location { block, statement_index }, target.ty(body, tcx).ty);
            }
        }
        if let TerminatorKind::Call { func, destination, .. } = &data.terminator().kind
            && let Some((callee, args)) = func.const_fn_def()
        {
            let (location, result) = (body.terminator_loc(block), destination.ty(body, tcx).ty);
            if cell_references(cx, env, result).next().is_some() && stays(cx, env, body, func) {
                anchor(location, result);
            } else if result.ty_adt_def().is_some_and(|it| cx.is_guard(it))
                || (tcx.trait_of_assoc(callee) == tcx.lang_items().deref_trait()
                    && args.type_at(0).ty_adt_def().is_some_and(|it| it.did() == cx.engine.gc))
            {
                releases.insert(location);
            }
        }
    }
    let flow = flow_except_at(&anchors);
    let borrows_of_locals = (facts.borrow_set.iter())
        .filter(|it| is_owned_by_local(cx, body, it.borrowed_place()))
        .filter(|it| cx.lends(env, erased(cx, env, it.borrowed_place().ty(body, tcx).ty)))
        .map(|it| it.region());
    let not_anchored =
        flow.reached_from(universal.iter().copied().chain(borrows_of_locals), |_| false);
    let anchored = flow.reached_from(anchored_at_first, |_| false);
    let is_anchored = |region| anchored.contains(&region) && !not_anchored.contains(&region);

    let flow = flow_except_at(&releases);
    let returned: FxHashSet<_> = all_variables(body.return_ty()).into_iter().collect();
    let held_by: IndexVec<Local, Vec<RegionVid>> = (body.local_decls.iter())
        .map(|it| variables(loose_regions(cx, env, it.ty, None)))
        .collect();
    let dropped_with: FxHashSet<(Local, RegionVid)> = input
        .drop_of_var_derefs_origin
        .iter()
        .map(|&(local, region)| (local, region.into()))
        .collect();
    let mut erased_body = None;
    for source in sources {
        let result = variables(loose_regions(cx, env, source.result, Some(&source.through)));
        let mut through = outer_regions(source.through.iter().copied());
        through.retain(|&it| !is_anchored(it));
        let seeds = if source.through.is_empty() {
            result
        } else if source.location != body.terminator_loc(source.location.block) {
            all_variables(source.result).into_iter().take(through.len()).collect()
        } else {
            let here = constraints().filter(|it| it.2 == source.location);
            let here = Flow::new(here.map(|it| (it.0, it.1)));
            let tokens = outer_regions(source.tokens.into_iter());
            let (loose, safe) =
                (here.reached_from(through, |_| false), here.reached_from(tokens, |_| false));
            result.into_iter().filter(|it| loose.contains(it) && !safe.contains(it)).collect()
        };
        if seeds.is_empty() {
            continue;
        }
        let loose =
            flow.reached_from(seeds, |it| universal.contains(&it) || returned.contains(&it));
        let mut escapes = loose.iter().any(|it| universal.contains(it));
        if loose.iter().any(|it| returned.contains(it)) {
            if tcx.is_closure_like(def_id.to_def_id()) {
                closures.returns_loose_borrow.insert(def_id.to_def_id());
            } else {
                escapes |= !callers_can_tell(cx, env, body, &loose);
            }
        }
        let holds = |local: Local| held_by[local].iter().any(|it| loose.contains(it));
        let reads_when_dropped =
            |local: Local| loose.iter().any(|&it| dropped_with.contains(&(local, it)));
        let held_at_end_of = blocks_that_hold(body, holds, reads_when_dropped);
        if escapes || !held_at_end_of.is_empty() {
            suspects.push(Suspect {
                def_id,
                body: erased_body.get_or_insert_with(|| {
                    &*tcx.arena.alloc(tcx.erase_and_anonymize_regions(body.clone()))
                }),
                span: body.source_info(source.location).span,
                ty: erased(cx, env, source.result),
                held_at_end_of,
                escapes,
            });
        }
    }
}

fn is_owned_by_local<'tcx>(cx: &Types<'tcx>, body: &Body<'tcx>, place: Place<'tcx>) -> bool {
    place.iter_projections().all(|(base, projection)| {
        projection != ProjectionElem::Deref || base.ty(body, cx.tcx).ty.is_box()
    })
}

/// Whether a call looks like a source.
fn callers_can_tell<'tcx>(
    cx: &Types<'tcx>,
    env: TypingEnv<'tcx>,
    body: &Body<'tcx>,
    loose: &FxHashSet<RegionVid>,
) -> bool {
    let parameters = body.args_iter().map(|it| body.local_decls[it].ty);
    let given: Vec<_> = parameters.flat_map(|it| cell_references(cx, env, it)).collect();
    let seen = variables(loose_regions(cx, env, body.return_ty(), Some(&given)));
    let held = variables(loose_regions(cx, env, body.return_ty(), None));
    !given.is_empty() && held.iter().filter(|it| loose.contains(it)).all(|it| seen.contains(it))
}

/// Those whose terminator is given such a local or is followed by a use of it.
fn blocks_that_hold(
    body: &Body<'_>,
    holds: impl Fn(Local) -> bool,
    reads_when_dropped: impl Fn(Local) -> bool,
) -> Vec<BasicBlock> {
    struct Transfer<'a> {
        tracked: &'a DenseBitSet<Local>,
        dropped: &'a DenseBitSet<Local>,
        live: DenseBitSet<Local>,
    }

    impl<'tcx> Visitor<'tcx> for Transfer<'_> {
        fn visit_place(&mut self, place: &Place<'tcx>, context: PlaceContext, _: Location) {
            use {MutatingUseContext::*, PlaceContext::*};
            match context {
                MutatingUse(Store | Call) | NonUse(NonUseContext::StorageLive)
                    if place.projection.is_empty() =>
                {
                    self.live.remove(place.local);
                }
                MutatingUse(Drop) if !self.dropped.contains(place.local) => {}
                NonUse(_) => {}
                _ if self.tracked.contains(place.local) => _ = self.live.insert(place.local),
                _ => {}
            }
        }

        fn visit_local(&mut self, local: Local, context: PlaceContext, location: Location) {
            self.visit_place(&local.into(), context, location);
        }
    }

    let set_of = |is_member: &dyn Fn(Local) -> bool| {
        let mut set = DenseBitSet::new_empty(body.local_decls.len());
        for local in body.local_decls.indices().filter(|&it| is_member(it)) {
            set.insert(local);
        }
        set
    };
    let (tracked, dropped) = (set_of(&holds), set_of(&reads_when_dropped));
    let nothing = DenseBitSet::new_empty(body.local_decls.len());
    let mut live_at_start = IndexVec::from_elem_n(nothing.clone(), body.basic_blocks.len());
    let mut found = DenseBitSet::new_empty(body.basic_blocks.len());
    let mut has_changed = true;
    while std::mem::take(&mut has_changed) {
        for &block in body.basic_blocks.reverse_postorder().iter().rev() {
            let data = &body[block];
            if data.is_cleanup {
                continue;
            }
            let mut transfer =
                Transfer { tracked: &tracked, dropped: &dropped, live: nothing.clone() };
            for successor in data.terminator().successors() {
                transfer.live.union(&live_at_start[successor]);
            }
            match &data.terminator().kind {
                TerminatorKind::Call { destination, func, args, .. } => {
                    let location = body.terminator_loc(block);
                    let context = PlaceContext::MutatingUse(MutatingUseContext::Call);
                    transfer.visit_place(destination, context, location);
                    transfer.visit_operand(func, location);
                    for Spanned { node, .. } in args {
                        transfer.visit_operand(node, location);
                    }
                }
                _ => transfer.visit_terminator(data.terminator(), body.terminator_loc(block)),
            }
            if !transfer.live.is_empty() {
                found.insert(block);
            }
            for (statement_index, statement) in data.statements.iter().enumerate().rev() {
                transfer.visit_statement(statement, Location { block, statement_index });
            }
            has_changed |= live_at_start[block].union(&transfer.live);
        }
    }
    body.basic_blocks.reverse_postorder().iter().copied().filter(|&it| found.contains(it)).collect()
}

/// The functions whose callers are held to the rule, and those that are taken at their word.
pub fn list_lenders<'tcx>(cx: &Types<'tcx>) {
    let tcx = cx.tcx;
    for def_id in tcx.hir_body_owners().map(|it| it.to_def_id()) {
        if !matches!(tcx.def_kind(def_id), DefKind::Fn | DefKind::AssocFn) {
            continue;
        }
        let env = TypingEnv::post_analysis(tcx, def_id);
        let signature = tcx.fn_sig(def_id).instantiate_identity().skip_normalization();
        let signature = tcx.liberate_late_bound_regions(def_id, signature);
        let inputs = || signature.inputs().iter().copied();
        let given: Vec<_> = inputs().flat_map(|it| cell_references(cx, env, it)).collect();
        let is_tied_to = |types: &[Ty<'tcx>], region| {
            types.iter().any(|it| matches!(*it.kind(), ty::Ref(own, ..) if own == region))
        };
        let tokens: Vec<_> = inputs().filter(|&it| is_no_gc(cx, it)).collect();
        if (loose_regions(cx, env, signature.output(), Some(&given)).into_iter())
            .any(|it| is_tied_to(&given, it) && !is_tied_to(&tokens, it))
        {
            let how = if cx.allows.stays(tcx, def_id) { "stays" } else { "lends" };
            let message = format!("{how} `{}`", signature.output());
            cx.report("lender", tcx.def_span(def_id).source_callsite(), message).emit();
        }
    }
}

fn is_no_gc(cx: &Types<'_>, ty: Ty<'_>) -> bool {
    ty.is_ref() && ty.peel_refs().ty_adt_def().is_some_and(|it| it.did() == cx.engine.no_gc)
}

/// Whatever the type parameters turn out to be.
fn may_collect_at<'a, 'tcx>(
    calls: &Calls<'a, 'tcx>,
    frame: Frame<'a, 'tcx>,
    terminator: &Terminator<'tcx>,
) -> Option<Way<'tcx>> {
    if let TerminatorKind::Call { args, .. } = &terminator.kind
        && args.iter().any(|it| frame.cx.has_callback(frame.env, frame.operand_type(&it.node)))
        && !calls.is_inside_collection(frame.instance)
    {
        return Some(Way::At(terminator.source_info.span));
    }
    calls.may_collect_at(frame, terminator)
}

pub fn report<'tcx>(
    calls: &Calls<'_, 'tcx>,
    cx: &Types<'tcx>,
    instantiations: &Instantiations<'tcx>,
    suspects: Vec<Suspect<'tcx>>,
) {
    let tcx = cx.tcx;
    let mut reported = FxHashSet::default();
    for suspect in suspects {
        let (def_id, body) = (suspect.def_id.to_def_id(), suspect.body);
        let env = TypingEnv::fully_monomorphized();
        let instances = instantiations.get(&def_id).into_iter().flatten();
        let mut frames: Vec<_> =
            instances.map(|&instance| Frame { cx, instance, env, body }).collect();
        // Other crates do not have the lifetimes to go by.
        let root = tcx.typeck_root_def_id_local(suspect.def_id);
        if frames.is_empty() || (calls.is_generic(def_id) && tcx.reachable_set(()).contains(&root))
        {
            let (instance, env) = (calls.as_declared(def_id), env_of(cx, body));
            frames.push(Frame { cx, instance, env, body });
        }
        let deferred = deferred_blocks(cx, body);
        let held = (suspect.held_at_end_of.iter().filter(|&&it| !deferred.contains(it)))
            .map(|&it| body[it].terminator())
            .flat_map(|it| frames.iter().map(move |&frame| (it, frame)))
            .find_map(|(it, frame)| Some((it.source_info.span, may_collect_at(calls, frame, it)?)));
        let escapes = suspect.escapes && !cx.allows.stays(tcx, def_id);
        let span = suspect.span.source_callsite();
        if (held.is_none() && !escapes)
            || cx.allows.covers_body(tcx, def_id)
            || !reported.insert(span)
        {
            continue;
        }
        let message =
            format!("`{}` does not keep the cell alive that it is borrowed from", suspect.ty);
        let mut diagnostic = cx.report("borrow", span, message);
        if let Some((span, way)) = held {
            diagnostic.span_label(span.source_callsite(), "held while this may collect");
            calls.explain(way, &mut diagnostic);
        } else {
            diagnostic.note("it leaves the function, whose signature does not show that it borrows from a `&` of a cell");
        }
        diagnostic.with_help("borrow it through `Gc::anchor` or `anchor!`").emit();
    }
}
