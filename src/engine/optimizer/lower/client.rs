//! The machine code around what the back end emits: the prologue, the exits, the calls and what they record.

use bun_engine_asm::prelude::*;
use bun_engine_asm::{Code, MacroAssembler, Target as CallTarget};
use bun_engine_codegen::ir::{Function, Location, Origin, Token};
use bun_engine_codegen::{
    CallSite as CallEnd, Client, Compiled, ExitSite, Frame, PatchpointSite, StateLocations,
};
use bun_engine_vm::{CallFrame, Vm, call};

use super::output::{
    CallSite, EXIT_REGISTER, ExitRecord, ExitValue, InvalidationPoint, Output, Place, symbol,
};
use super::sites::{Site, Sites};
use crate::graph::{Entry, ExitKind, Format, Graph, NO_LAYOUT, Position};

/// The prologue runs where the parameters are still in the places they arrived in, so it keeps off those.
const PROLOGUE_SCRATCH: Gpr = abi::NON_ARGUMENT_GPRS[0];
const PROLOGUE_TEMP: Gpr = abi::NON_ARGUMENT_GPRS[1];

fn in_frame(offset: i32) -> Address {
    Address::new(abi::FRAME_POINTER, offset)
}

/// An exit whose code is placed but whose offset is not known yet.
struct PendingExit {
    kind: ExitKind,
    position: Position,
    layout: u32,
    first_value: u32,
    value_count: u32,
    start: Label,
}

struct PendingCall {
    site: u32,
    return_address: Label,
    position: Position,
    layout: u32,
    first_value: u32,
    value_count: u32,
}

/// What the client gathers while it emits, which the tables are built from.
#[derive(Default)]
pub(super) struct Tables {
    exits: Vec<PendingExit>,
    exit_values: Vec<ExitValue>,
    calls: Vec<PendingCall>,
    call_values: Vec<ExitValue>,
    /// The bytes to write a jump over, and the exit to jump to.
    invalidation_points: Vec<(Label, Label)>,
    overflow: Option<Label>,
    pad_arguments: Option<Label>,
    body: Option<Label>,
}

impl Tables {
    fn clear(&mut self) {
        self.exits.clear();
        self.exit_values.clear();
        self.calls.clear();
        self.call_values.clear();
        self.invalidation_points.clear();
        (self.overflow, self.pad_arguments, self.body) = (None, None, None);
    }
}

pub(super) struct Emitter<'a> {
    graph: &'a Graph,
    sites: &'a Sites,
    tables: &'a mut Tables,
    /// How far below the frame pointer the frames that an exit rebuilds reach.
    rebuilt_frame_bytes: u32,
}

impl<'a> Emitter<'a> {
    pub(super) fn new(graph: &'a Graph, sites: &'a Sites, tables: &'a mut Tables) -> Self {
        tables.clear();
        let deepest = (graph.frames.iter())
            .map(|frame| (frame.frame_words + frame.parameters + 8) * 8)
            .max()
            .unwrap_or(0);
        Self {
            graph,
            sites,
            tables,
            rebuilt_frame_bytes: deepest,
        }
    }
}

/// The tables, once the code has its offsets and the emitter is done with them.
pub(super) fn finish(
    tables: &mut Tables,
    code: Code,
    compiled: &Compiled,
    mut graph: Graph,
) -> Output {
    {
        let at = |label: Label| code.offset_of(label);
        let exits = (tables.exits.iter())
            .map(|exit| ExitRecord {
                kind: exit.kind,
                offset: at(exit.start),
                frame: exit.position.frame,
                bytecode_offset: exit.position.offset,
                layout: exit.layout,
                first_value: exit.first_value,
                value_count: exit.value_count,
            })
            .collect();
        let call_sites = (tables.calls.iter())
            .map(|call| CallSite {
                site: call.site,
                return_offset: at(call.return_address),
                frame: call.position.frame,
                bytecode_offset: call.position.offset,
                layout: call.layout,
                first_value: call.first_value,
                value_count: call.value_count,
            })
            .collect();
        let invalidation_points = (tables.invalidation_points.iter())
            .map(|&(point, target)| InvalidationPoint {
                offset: at(point),
                target: at(target),
            })
            .collect();
        let weak_references = graph.cells.keys().map(|&bits| bits as usize).collect();
        let positions = graph.sites.iter().map(|site| site.position).collect();
        Output {
            entry: at(compiled.emitted.entries[0]),
            frame_size: compiled.frame.size,
            call_sites,
            call_site_values: core::mem::take(&mut tables.call_values),
            exits,
            exit_values: core::mem::take(&mut tables.exit_values),
            handlers: Vec::new(),
            invalidation_points,
            weak_references,
            assumptions: core::mem::take(&mut graph.assumptions),
            frames: core::mem::take(&mut graph.frames),
            layouts: core::mem::take(&mut graph.layouts),
            layout_registers: core::mem::take(&mut graph.layout_registers),
            positions,
            code,
        }
    }
}

impl Emitter<'_> {
    fn position_of(&self, origin: Origin) -> Position {
        self.graph.site(origin).position
    }

    /// Copies out where the values of a state are, and gives the range they take.
    fn record_values(
        function: &Function,
        state: StateLocations<'_>,
        into: &mut Vec<ExitValue>,
    ) -> (u32, u32) {
        let first = into.len() as u32;
        let tags = function.state_tags(state.state());
        for (position, place) in state.iter().enumerate() {
            let format = tags
                .get(position)
                .map_or(Format::Tagged, |&tag| Format::from_tag(tag));
            into.push(ExitValue {
                place: place_of(place),
                format,
            });
        }
        (first, into.len() as u32 - first)
    }

    /// Records an exit and gives its number, which its code loads.
    fn add_exit(
        &mut self,
        function: &Function,
        kind: ExitKind,
        position: Position,
        state: StateLocations<'_>,
        start: Label,
    ) -> u32 {
        let (first_value, value_count) =
            Self::record_values(function, state, &mut self.tables.exit_values);
        let layout = if state.is_empty() {
            NO_LAYOUT
        } else {
            function.state_layout(state.state())
        };
        self.tables.exits.push(PendingExit {
            kind,
            position,
            layout,
            first_value,
            value_count,
            start,
        });
        self.tables.exits.len() as u32 - 1
    }

    /// Two instructions: the number of the exit, and a jump to the thunk that reads it.
    fn emit_exit_stub(&self, masm: &mut MacroAssembler, number: u32) {
        masm.move32(EXIT_REGISTER, number as i32);
        let thunk = masm.external(CallTarget::Symbol(symbol::EXIT));
        masm.jump(thunk);
    }

    fn overflow(&mut self, masm: &mut MacroAssembler) -> Label {
        *self.tables.overflow.get_or_insert_with(|| masm.new_label())
    }
}

fn place_of(location: Location) -> Place {
    match location {
        Location::Gpr(reg) => Place::Gpr(reg.number()),
        Location::Fpr(reg) => Place::Fpr(reg.number()),
        Location::Frame(offset) => Place::Frame(offset),
        Location::FrameAddress(offset) => Place::FrameAddress(offset),
        Location::Constant(bits) => Place::Constant(bits.get()),
        Location::VectorConstant(_) => unreachable!("no state holds a vector"),
    }
}

impl Client<MacroAssembler> for Emitter<'_> {
    fn prologue(&mut self, masm: &mut MacroAssembler, _: usize, frame: &Frame) {
        let bottom = in_frame(-(frame.size as i32));
        let limit = Address::new(abi::VM, Vm::OFFSET_OF_STACK_LIMIT as i32);
        if let Entry::Loop(_) = self.graph.entry {
            // The frame is the one that the lower tier built; only what is below it is ours.
            masm.compute_address(PROLOGUE_SCRATCH, bottom);
            let overflow = self.overflow(masm);
            masm.branch64(Cond::Below, PROLOGUE_SCRATCH, limit, overflow);
            return masm.move64(abi::STACK_POINTER, PROLOGUE_SCRATCH);
        }
        masm.enter_frame();
        let parameters = self.graph.frames[0].parameters;
        let body = *self.tables.body.get_or_insert_with(|| masm.new_label());
        if parameters > 1 {
            let pad = *self
                .tables
                .pad_arguments
                .get_or_insert_with(|| masm.new_label());
            let count = in_frame(CallFrame::OFFSET_OF_ARGUMENT_COUNT as i32);
            masm.branch32(Cond::Below, count, parameters as i32, pad);
        }
        masm.bind(body);
        masm.compute_address(PROLOGUE_SCRATCH, bottom);
        let overflow = self.overflow(masm);
        masm.branch64(Cond::Below, PROLOGUE_SCRATCH, limit, overflow);
        masm.move64(abi::STACK_POINTER, PROLOGUE_SCRATCH);
    }

    fn shared_exit(&mut self, _: &mut MacroAssembler, _: Token) -> Option<Label> {
        None
    }

    fn exit(&mut self, masm: &mut MacroAssembler, site: &ExitSite<'_>) {
        let info = self.graph.exits[site.token.0 as usize];
        let position = self.position_of(info.origin);
        let start = masm.here();
        let number = self.add_exit(site.function, info.kind, position, site.state, start);
        self.emit_exit_stub(masm, number);
    }

    fn patchpoint(&mut self, masm: &mut MacroAssembler, site: &PatchpointSite<'_>) {
        match self.sites.at(site.token) {
            Site::InvalidationPoint => {
                let stub = masm.new_label();
                let position = self.position_of(site.origin);
                let number = self.add_exit(
                    site.function,
                    ExitKind::Invalidated,
                    position,
                    site.state,
                    stub,
                );
                let point = masm.replaceable_point();
                self.tables.invalidation_points.push((point, stub));
                let _ = number;
            }
            Site::Publish => {
                let done = masm.new_label();
                masm.branch_if_zero32(site.operands[0].gpr(), done);
                masm.store_fence();
                masm.bind(done);
            }
            Site::Call(_) | Site::TailCall(_) => unreachable!("not emitted yet"),
            Site::Operation | Site::Leaf => {}
        }
    }

    fn before_call(&mut self, masm: &mut MacroAssembler, token: Token, _: Origin) {
        if let Site::Operation | Site::Call(_) = self.sites.at(token) {
            let site = self.tables.calls.len() as u32;
            masm.store32(in_frame(CallFrame::OFFSET_OF_CALL_SITE as i32), site as i32);
        }
    }

    fn after_call(&mut self, _: &mut MacroAssembler, site: &CallEnd<'_>) {
        if let Site::Leaf = self.sites.at(site.token) {
            return;
        }
        let (first_value, value_count) =
            Self::record_values(site.function, site.state, &mut self.tables.call_values);
        let layout = if site.state.is_empty() {
            NO_LAYOUT
        } else {
            site.function.state_layout(site.state.state())
        };
        let origin = self.sites.origin_of(site.token);
        self.tables.calls.push(PendingCall {
            site: self.tables.calls.len() as u32,
            return_address: site.return_address,
            position: self.position_of(origin),
            layout,
            first_value,
            value_count,
        });
    }

    fn out_of_line(&mut self, masm: &mut MacroAssembler) {
        if let Some(pad) = self.tables.pad_arguments {
            let body = self.tables.body.expect("the prologue has bound it");
            let parameters = self.graph.frames[0].parameters as i32;
            masm.bind(pad);
            // A generator that is resumed has no arguments at all, and wants none.
            let count = in_frame(CallFrame::OFFSET_OF_ARGUMENT_COUNT as i32);
            masm.branch32(Cond::Eq, count, 0, body);
            let overflow = self.overflow(masm);
            let limit = Address::new(abi::VM, Vm::OFFSET_OF_REAL_STACK_LIMIT as i32);
            let needed = self.rebuilt_frame_bytes as i32 + parameters * 8;
            masm.compute_address(PROLOGUE_TEMP, in_frame(-needed));
            masm.branch64(Cond::Below, PROLOGUE_TEMP, limit, overflow);
            masm.move32(abi::ARGUMENT_GPRS[0], parameters);
            masm.call_address(call::pad_arguments());
            masm.jump(body);
        }
        if let Some(overflow) = self.tables.overflow {
            masm.bind(overflow);
            masm.move64(abi::ARGUMENT_GPRS[0], abi::VM);
            masm.move64(abi::ARGUMENT_GPRS[1], abi::FRAME_POINTER);
            masm.call_address(crate::runtime::operations::address::throw_stack_overflow());
            masm.jump_address(call::throw_from_frame());
        }
    }
}
