//! The locals and temporaries of one function.

use rustc_data_structures::fx::FxHashSet;
use rustc_index::bit_set::DenseBitSet;
use rustc_index::{IndexSlice, IndexVec};
use rustc_middle::mir::visit::{
    MutatingUseContext, NonMutatingUseContext, NonUseContext, PlaceContext, Visitor,
};
use rustc_middle::mir::{
    BasicBlock, BasicBlockData, Body, BorrowKind, CastKind, Local, Location, Operand, Place,
    RETURN_PLACE, Rvalue, Terminator, TerminatorKind,
};
use rustc_middle::ty::adjustment::PointerCoercion;
use rustc_middle::ty::{
    self, EarlyBinder, Instance, InstanceKind, Ty, TypeVisitableExt, TypingEnv,
};
use rustc_span::{Span, Spanned, Symbol};

use crate::calls::Calls;
use crate::types::{Hidden, Types};

pub const HELP: &str = "use `ValueVec`, `RootVec`, `RootMap`, `RootSet`, `RootDeque` or `RootBox`, or hold the cells by `Strong`";

impl Hidden<'_> {
    pub fn message(self) -> String {
        format!(
            "`{}` hides references to cells from the collector",
            self.container
        )
    }
}

pub struct Finding {
    pub rule: &'static str,
    pub span: Span,
    pub message: String,
    pub cause: Option<(Span, &'static str)>,
    pub help: &'static str,
}

/// A function with its type parameters filled in, or left as they are.
#[derive(Clone, Copy)]
pub struct Frame<'a, 'tcx> {
    pub cx: &'a Types<'tcx>,
    pub instance: Instance<'tcx>,
    pub env: TypingEnv<'tcx>,
    pub body: &'tcx Body<'tcx>,
}

impl<'tcx> Frame<'_, 'tcx> {
    pub fn instantiate(&self, ty: Ty<'tcx>) -> Ty<'tcx> {
        if !ty.has_param() && !ty.has_aliases() {
            return ty;
        }
        let (tcx, bound) = (self.cx.tcx, EarlyBinder::bind(self.cx.tcx, ty));
        (self
            .instance
            .try_instantiate_mir_and_normalize_erasing_regions(tcx, self.env, bound))
        .unwrap_or(ty)
    }

    pub fn operand_type(&self, operand: &Operand<'tcx>) -> Ty<'tcx> {
        self.instantiate(operand.ty(self.body, self.cx.tcx))
    }

    pub fn place_type(&self, place: Place<'tcx>) -> Ty<'tcx> {
        self.instantiate(place.ty(self.body, self.cx.tcx).ty)
    }

    fn hides(&self, ty: Ty<'tcx>) -> Option<Hidden<'tcx>> {
        self.cx.hides(self.env, ty)
    }

    fn refers(&self, ty: Ty<'tcx>) -> bool {
        self.cx.refers(self.env, ty)
    }

    /// The code of this repository only hands such values on. Other code may do with them what it likes for as long
    /// as nothing can collect.
    fn is_strict(&self) -> bool {
        matches!(self.instance.def, InstanceKind::Item(it) if self.cx.engine.is_in_workspace(it))
    }

    pub fn blocks(self) -> impl Iterator<Item = (BasicBlock, &'tcx BasicBlockData<'tcx>)> {
        self.body
            .basic_blocks
            .iter_enumerated()
            .filter(|(_, data)| !data.is_cleanup)
    }
}

pub fn check<'a, 'tcx>(frame: Frame<'a, 'tcx>, calls: &Calls<'a, 'tcx>) -> Vec<Finding> {
    let hidden = frame.body.local_decls.iter().map(|local| {
        let may_own = !local.ty.is_any_ptr() && !local.ty.is_scalar() && !local.ty.is_unit();
        may_own
            .then(|| frame.hides(frame.instantiate(local.ty)))
            .flatten()
    });
    let mut uses = Uses {
        frame,
        hidden: hidden.collect(),
        borrowed: DenseBitSet::new_empty(frame.body.local_decls.len()),
        glances: FxHashSet::default(),
        findings: Vec::new(),
    };
    uses.forget_box_aliases();
    uses.find_glances();
    for (block, data) in frame.blocks() {
        uses.visit_basic_block_data(block, data);
    }
    if uses.hidden.iter().any(Option::is_some) {
        uses.check_return();
        uses.check_held_across_collection(calls);
    }
    uses.findings
}

struct Uses<'a, 'tcx> {
    frame: Frame<'a, 'tcx>,
    /// Taken when it is reported.
    hidden: IndexVec<Local, Option<Hidden<'tcx>>>,
    borrowed: DenseBitSet<Local>,
    /// Borrows for no more than `len`, `is_some` and the like.
    glances: FxHashSet<Location>,
    findings: Vec<Finding>,
}

impl<'a, 'tcx> Uses<'a, 'tcx> {
    fn span(&self, location: Location) -> Span {
        self.frame.body.source_info(location).span
    }

    fn report_local(&mut self, local: Local, cause: Span, label: &'static str) {
        if let Some(hidden) = self.hidden[local].take() {
            self.findings.push(Finding {
                rule: "hidden",
                span: self.frame.body.local_decls[local].source_info.span,
                message: hidden.message(),
                cause: Some((cause, label)),
                help: HELP,
            });
        }
    }

    fn report(&mut self, rule: &'static str, span: Span, message: String, help: &'static str) {
        self.findings.push(Finding {
            rule,
            span,
            message,
            cause: None,
            help,
        });
    }

    /// The local that owns `place`, if what is there hides something.
    fn owner(&self, place: Place<'tcx>) -> Option<Local> {
        let owns = self.hidden[place.local].is_some()
            && !place.is_indirect()
            && self.frame.hides(self.frame.place_type(place)).is_some();
        owns.then_some(place.local)
    }

    /// To look into a box that is somewhere else, the compiler copies it to a temporary.
    fn forget_box_aliases(&mut self) {
        for statement in self.frame.blocks().flat_map(|(_, data)| &data.statements) {
            if let Some((target, Rvalue::Use(Operand::Copy(source), _))) =
                statement.kind.as_assign()
                && let Some(target) = target.as_local()
                && self.frame.is_strict()
                && source.as_local().is_none()
            {
                self.hidden[target] = None;
            }
        }
    }

    fn find_glances(&mut self) {
        for (block, data) in self.frame.blocks() {
            if let TerminatorKind::Call {
                func,
                args,
                destination,
                ..
            } = &data.terminator().kind
                && let [
                    Spanned {
                        node: Operand::Move(argument),
                        ..
                    },
                ] = **args
                && let Some((callee, _)) = func.const_fn_def()
                && !self.frame.cx.engine.is_in_workspace(callee)
                && self.frame.cx.tcx.inherent_impl_of_assoc(callee).is_some()
                && self.frame.place_type(*destination).is_scalar()
                && let Some(statement_index) = data.statements.iter().rposition(|it| {
                    matches!(
                        it.kind.as_assign(),
                        Some((target, Rvalue::Ref(_, BorrowKind::Shared, _))) if *target == argument
                    )
                })
            {
                self.glances.insert(Location {
                    block,
                    statement_index,
                });
            }
        }
    }

    fn check_cast(&mut self, kind: CastKind, operand: &Operand<'tcx>, to: Ty<'tcx>, span: Span) {
        let frame = self.frame;
        match kind {
            CastKind::BoxDerefTransmute if frame.is_strict() && !is_from_vec_macro(frame, span) => {
                if let Some(place) = operand.place() {
                    self.report_local(place.local, span, "looked into here");
                }
            }
            CastKind::PointerCoercion(PointerCoercion::Unsize, _) => {
                let (from, to) = (frame.operand_type(operand), frame.instantiate(to));
                if let Some(local) = operand.place().and_then(|it| self.owner(it)) {
                    if frame.hides(to).is_none() {
                        self.report_local(local, span, "made a trait object here");
                    }
                } else if let (ty::Ref(_, cell, _), ty::Ref(_, object, _)) =
                    (from.kind(), to.kind())
                    && frame.is_strict()
                    && object.is_trait()
                    && !cell.is_trait()
                    && frame.cx.is_cell(*cell)
                {
                    let message = format!("`{to}` does not tell that it refers to a cell");
                    self.report(
                        "erased",
                        span,
                        message,
                        "make the trait object from a `&Gc<T>`",
                    );
                }
            }
            _ => {}
        }
    }

    fn check_call(
        &mut self,
        function: &Operand<'tcx>,
        arguments: &[Spanned<Operand<'tcx>>],
        result: Place<'tcx>,
        span: Span,
    ) {
        let frame = self.frame;
        let (cx, tcx) = (frame.cx, frame.cx.tcx);
        let Some((callee, callee_args)) = function.const_fn_def() else {
            return;
        };
        if !frame.is_strict() || cx.engine.is_upper(callee) {
            return;
        }
        let result = frame.place_type(result);
        if !frame.refers(result) {
            return;
        }
        if result.ty_adt_def().is_some_and(|it| it.is_manually_drop())
            && let [value] = arguments
            && let value = frame.operand_type(&value.node)
            && value
                .ty_adt_def()
                .is_some_and(|it| it.has_dtor(tcx) && cx.engine.is_upper(it.did()))
        {
            let message = format!("`{value}` keeps its cell alive only if it is dropped");
            return self.report("guard", span, message, "let it go out of scope");
        }
        if frame.hides(result).is_some() {
            return;
        }
        let types = || arguments.iter().map(|it| frame.operand_type(&it.node));
        if !types().any(|it| frame.refers(it) && frame.hides(it).is_none()) {
            let before = self.findings.len();
            for place in arguments.iter().filter_map(|it| it.node.place()) {
                if let Some(local) = self.owner(place) {
                    self.report_local(local, span, "taken apart or leaked here");
                }
            }
            if self.findings.len() != before {
                return;
            }
        }
        // As `Arena::alloc<T>(&self, T) -> &mut T`.
        let signature = tcx.fn_sig(callee).instantiate_identity().skip_binder();
        let instantiate = |ty: Ty<'tcx>| {
            let ty = EarlyBinder::bind(tcx, ty)
                .instantiate(tcx, callee_args)
                .skip_normalization();
            let ty = tcx.instantiate_bound_regions_with_erased(ty::Binder::bind_with_vars(
                ty,
                tcx.fn_sig(callee).skip_binder().bound_vars(),
            ));
            frame.instantiate(cx.normalize(frame.env, ty::Unnormalized::new(ty)))
        };
        let is_lent_by_nothing_that_refers = |ty: Ty<'tcx>| {
            let ty::Ref(region, pointee, _) = *ty.kind() else {
                return false;
            };
            let lends = |input: &Ty<'tcx>| input.walk().any(|it| it.as_region() == Some(region));
            frame.refers(instantiate(pointee))
                && !(signature.inputs().iter().copied().filter(lends))
                    .any(|input| frame.refers(instantiate(input)))
        };
        if signature.safety().is_safe()
            && signature
                .output()
                .walk()
                .filter_map(|it| it.as_type())
                .any(is_lent_by_nothing_that_refers)
            && types().any(|ty| {
                ty.walk()
                    .filter_map(|it| it.as_type())
                    .any(|it| frame.refers(it))
            })
        {
            let message = format!("`{result}` points to memory that the collector does not know");
            self.report("hidden", span, message, HELP);
        }
    }

    fn check_return(&mut self) {
        let (tcx, def_id) = (self.frame.cx.tcx, self.frame.instance.def_id());
        if self.frame.is_strict()
            && tcx.def_kind(def_id).is_fn_like()
            && !tcx.is_closure_like(def_id)
            && !tcx.fn_sig(def_id).skip_binder().abi().is_rustic_abi()
        {
            self.report_local(
                RETURN_PLACE,
                tcx.def_span(def_id),
                "returned to foreign code",
            );
        }
    }

    fn check_held_across_collection(&mut self, calls: &Calls<'a, 'tcx>) {
        let body = self.frame.body;
        let nothing = DenseBitSet::new_empty(body.local_decls.len());
        let mut live_at_start = IndexVec::from_elem_n(nothing, body.basic_blocks.len());
        let mut has_changed = true;
        while std::mem::take(&mut has_changed) {
            for &block in body.basic_blocks.reverse_postorder().iter().rev() {
                let data = &body[block];
                let live = self.live_after(data.terminator(), &live_at_start);
                let mut transfer = Liveness { uses: self, live };
                transfer.visit_terminator(data.terminator(), body.terminator_loc(block));
                for (statement_index, statement) in data.statements.iter().enumerate().rev() {
                    transfer.visit_statement(
                        statement,
                        Location {
                            block,
                            statement_index,
                        },
                    );
                }
                has_changed |= !data.is_cleanup && live_at_start[block].union(&transfer.live);
            }
        }
        for (_, data) in self.frame.blocks() {
            let terminator = data.terminator();
            let live = self.live_after(terminator, &live_at_start);
            if !live.is_empty() && calls.may_collect_at(self.frame, terminator) {
                for local in live.iter() {
                    let span = terminator.source_info.span;
                    self.report_local(local, span, "held while this may collect");
                }
            }
        }
    }

    fn live_after(
        &self,
        terminator: &Terminator<'tcx>,
        live_at_start: &IndexSlice<BasicBlock, DenseBitSet<Local>>,
    ) -> DenseBitSet<Local> {
        let mut live = DenseBitSet::new_empty(self.hidden.len());
        for successor in terminator.successors() {
            live.union(&live_at_start[successor]);
        }
        if let TerminatorKind::Call { destination, .. } = &terminator.kind
            && let Some(local) = destination.as_local()
        {
            live.remove(local);
        }
        live
    }
}

impl<'tcx> Visitor<'tcx> for Uses<'_, 'tcx> {
    fn visit_place(&mut self, place: &Place<'tcx>, context: PlaceContext, location: Location) {
        let is_copy = context == PlaceContext::NonMutatingUse(NonMutatingUseContext::Copy);
        if (is_copy || context.is_borrow() || context.is_address_of())
            && !self.glances.contains(&location)
            && let Some(local) = self.owner(*place)
        {
            self.borrowed.insert(local);
            if self.frame.is_strict() {
                let label = if is_copy {
                    "looked into here"
                } else {
                    "borrowed here"
                };
                self.report_local(local, self.span(location), label);
            }
        }
    }

    fn visit_rvalue(&mut self, rvalue: &Rvalue<'tcx>, location: Location) {
        if let Rvalue::Cast(kind, operand, to) = rvalue {
            self.check_cast(*kind, operand, *to, self.span(location));
        }
        self.super_rvalue(rvalue, location);
    }

    fn visit_terminator(&mut self, terminator: &Terminator<'tcx>, location: Location) {
        if let TerminatorKind::Call {
            func,
            args,
            destination,
            ..
        } = &terminator.kind
        {
            self.check_call(func, args, *destination, terminator.source_info.span);
        }
        self.super_terminator(terminator, location);
    }
}

/// Backwards: what is still to be read. Dropping reads nothing, but it is where a borrow ends at the latest.
struct Liveness<'a, 'b, 'tcx> {
    uses: &'a Uses<'b, 'tcx>,
    live: DenseBitSet<Local>,
}

impl<'tcx> Visitor<'tcx> for Liveness<'_, '_, 'tcx> {
    fn visit_place(&mut self, place: &Place<'tcx>, context: PlaceContext, location: Location) {
        use {MutatingUseContext::*, PlaceContext::*};
        match context {
            _ if self.uses.glances.contains(&location) => {}
            MutatingUse(Store) | NonUse(NonUseContext::StorageLive) => {
                if let Some(local) = place.as_local() {
                    self.live.remove(local);
                }
            }
            MutatingUse(Call | AsmOutput | SetDiscriminant) | NonUse(_) => {}
            NonMutatingUse(NonMutatingUseContext::Inspect) => {}
            MutatingUse(Drop) if !self.uses.borrowed.contains(place.local) => {}
            _ => {
                if let Some(local) = self.uses.owner(*place) {
                    self.live.insert(local);
                }
            }
        }
    }

    fn visit_local(&mut self, local: Local, context: PlaceContext, location: Location) {
        self.visit_place(&local.into(), context, location);
    }
}

/// It writes the elements through the box that becomes the `Vec`.
fn is_from_vec_macro(frame: Frame<'_, '_>, span: Span) -> bool {
    let definition = span.ctxt().outer_expn_data().macro_def_id;
    definition.is_some_and(|it| {
        frame
            .cx
            .tcx
            .is_diagnostic_item(Symbol::intern("vec_macro"), it)
    })
}
