//! A function on its way from bytecode to machine code. It refers to cells by address only.

mod data;
mod facts;
mod frames;
pub(crate) mod heaps;
mod knowledge;
mod node;

use bun_engine_asm::{RegisterSet, abi};
use bun_engine_bytecode::Register;
use bun_engine_codegen::ir::{Exit, Function, Op, Origin, State, Table, Token, Value as Ssa};
use bun_engine_codegen::passes::Level;
use bun_engine_vm::{ExitKinds, FastHashMap, RealmWatchpoint, SpeculatedType, Value};
pub(crate) use data::*;
pub(crate) use facts::*;
pub(crate) use frames::*;
pub(crate) use knowledge::{Knowledge, Known, SavedKnowledge, implied_type};
pub(crate) use node::{Gives, Kind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Entry {
    Function,
    /// Takes over the frame of code that is not optimized at the loop with this offset.
    Loop(u32),
}

/// What the code may count on without checking.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Assumption {
    Stable(ShapeIndex),
    Realm(usize, RealmWatchpoint),
}

#[derive(Clone, Copy)]
pub(crate) struct Site {
    pub(crate) position: Position,
    pub(crate) exits: ExitKinds,
    /// The address of the entry of the instruction, or zero.
    pub(crate) metadata: usize,
    /// An exception may be caught in this frame on the stack.
    pub(crate) has_handler: bool,
}

pub(crate) struct Graph {
    pub(crate) function: Function,
    pub(crate) level: Level,
    pub(crate) entry: Entry,
    pub(crate) frames: Vec<FrameInfo>,
    /// By origin.
    pub(crate) sites: Vec<Site>,
    pub(crate) layouts: Vec<Layout>,
    pub(crate) layout_registers: Vec<Register>,
    pub(crate) exits: Vec<ExitInfo>,
    pub(crate) shapes: Shapes,
    pub(crate) cells: FastHashMap<u64, CellFacts>,
    pub(crate) transitions: Vec<Transition>,
    pub(crate) allocations: Vec<Allocation>,
    pub(crate) shadow_frame: ShadowFrame,
    /// What tagged values are expected to be. Empty for what has never been seen.
    pub(crate) predictions: Table<Ssa, SpeculatedType>,
    pub(crate) types: TypeChecks,
    /// By realm, a bit for each watchpoint that held when the graph was built.
    pub(crate) realms: Vec<(usize, u64)>,
    /// Those that passes have made use of.
    pub(crate) assumptions: Vec<Assumption>,
}

pub(crate) const PINNED: RegisterSet = abi::PINNED;

impl Graph {
    pub(crate) fn new(level: Level, entry: Entry) -> Self {
        let mut function = Function::new(PINNED);
        function.set_node_kinds(node::INFO);
        function.set_pinned_value(abi::NUMBER_TAG, Value::NUMBER_TAG);
        function.set_pinned_value(abi::NOT_CELL_MASK, Value::NOT_CELL_MASK);
        heaps::add_fixed(&mut function);
        Self {
            function,
            level,
            entry,
            frames: Vec::new(),
            sites: Vec::new(),
            layouts: Vec::new(),
            layout_registers: Vec::new(),
            exits: Vec::new(),
            shapes: Shapes::default(),
            cells: FastHashMap::default(),
            transitions: Vec::new(),
            allocations: Vec::new(),
            shadow_frame: ShadowFrame::default(),
            predictions: Table::default(),
            types: TypeChecks::default(),
            realms: Vec::new(),
            assumptions: Vec::new(),
        }
    }

    #[inline]
    pub(crate) fn node(&self, value: Ssa) -> Option<(Kind, u32)> {
        self.function
            .op(value)
            .node()
            .map(|(kind, data)| (Kind::of(kind), data))
    }

    #[inline]
    pub(crate) fn kind(&self, value: Ssa) -> Option<Kind> {
        self.node(value).map(|(kind, _)| kind)
    }

    #[inline]
    pub(crate) fn operand(&self, value: Ssa, position: usize) -> Ssa {
        self.function
            .resolve(self.function.operands(value)[position])
    }

    /// The operand of a node of that kind.
    #[inline]
    pub(crate) fn inside(&self, value: Ssa, kind: Kind) -> Option<Ssa> {
        (self.kind(value) == Some(kind)).then(|| self.operand(value, 0))
    }

    /// The value without the checks that it has passed.
    #[inline]
    pub(crate) fn root(&self, mut value: Ssa) -> Ssa {
        value = self.function.resolve(value);
        while self
            .kind(value)
            .is_some_and(|kind| matches!(kind.gives(), Gives::Operand))
        {
            value = self.operand(value, 0);
        }
        value
    }

    pub(crate) fn site(&self, origin: Origin) -> &Site {
        &self.sites[origin.0 as usize]
    }

    pub(crate) fn frame_at(&self, origin: Origin) -> &FrameInfo {
        &self.frames[self.site(origin).position.frame.index()]
    }

    pub(crate) fn has_exited(&self, origin: Origin, kind: ExitKind) -> bool {
        self.site(origin).exits.contains(kind)
    }

    pub(crate) fn exit(&mut self, kind: ExitKind, origin: Origin, state: State) -> Exit {
        debug_assert!(!state.is_none());
        self.exits.push(ExitInfo { kind, origin });
        Exit {
            token: Token(self.exits.len() as u32 - 1),
            state,
        }
    }

    pub(crate) fn type_check(&mut self, check: TypeCheck) -> u32 {
        self.types.check(check)
    }

    pub(crate) fn type_check_at(&self, data: u32) -> TypeCheck {
        self.types.at(data)
    }

    pub(crate) fn intern_type(&mut self, wanted: SpeculatedType) -> u32 {
        self.type_check(TypeCheck {
            wanted,
            known: SpeculatedType::BYTECODE_TOP,
            failure: ExitKind::BadType,
        })
    }

    pub(crate) fn interned_type(&self, data: u32) -> SpeculatedType {
        self.type_check_at(data).wanted
    }

    pub(crate) fn prediction(&self, value: Ssa) -> SpeculatedType {
        if let Op::Const64(bits) = *self.function.op(value) {
            return self.type_of_constant(bits.get());
        }
        self.predictions.get(value).copied().unwrap_or_default()
    }

    pub(crate) fn predict(&mut self, value: Ssa, prediction: SpeculatedType) {
        self.predictions
            .grow_to(self.function.value_count(), SpeculatedType::empty());
        self.predictions[value] = prediction;
    }

    pub(crate) fn type_of_constant(&self, bits: u64) -> SpeculatedType {
        SpeculatedType::of_immediate(bits).unwrap_or_else(|| {
            self.cells
                .get(&bits)
                .map_or(SpeculatedType::CELL, |facts| facts.ty)
        })
    }

    /// The bits of a tagged constant.
    pub(crate) fn constant(&self, value: Ssa) -> Option<u64> {
        match *self.function.op(value) {
            Op::Const64(bits) => Some(bits.get()),
            _ => None,
        }
    }

    /// What follows from the instruction alone.
    pub(crate) fn inherent_type(&self, value: Ssa) -> SpeculatedType {
        match *self.function.op(value) {
            Op::Const64(bits) => self.type_of_constant(bits.get()),
            ref op => match op.node().map(|(kind, _)| Kind::of(kind).gives()) {
                Some(Gives::Value(ty)) => ty,
                _ => SpeculatedType::BYTECODE_TOP,
            },
        }
    }

    pub(crate) fn inherent_shapes(&self, value: Ssa) -> ShapeSet {
        self.constant(value)
            .and_then(|bits| self.cells.get(&bits))
            .map_or(ShapeSet::ANY, |facts| facts.shapes)
    }

    pub(crate) fn assume(&mut self, assumption: Assumption) {
        if !self.assumptions.contains(&assumption) {
            self.assumptions.push(assumption);
        }
    }

    pub(crate) fn assume_stable(&mut self, set: ShapeSet) {
        for position in 0..self.shapes.members(set).len() {
            self.assume(Assumption::Stable(self.shapes.members(set)[position]));
        }
    }

    /// Whether it holds in the realm of the code at `origin`, in which case the code counts on it from now on.
    pub(crate) fn try_to_assume(&mut self, origin: Origin, which: RealmWatchpoint) -> bool {
        let global = self.frame_at(origin).global;
        let holds = self
            .realms
            .iter()
            .any(|&(realm, holding)| realm == global && holding >> which as u32 & 1 != 0);
        if holds {
            self.assume(Assumption::Realm(global, which));
        }
        holds
    }
}
