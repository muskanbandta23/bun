//! Copies blocks to the ends of others, and then gives each use of a copied value the copy that reaches it.

use bun_engine_asm::Sign;

use super::Analyses;
use super::reaching::{Held, Reaching, Read};
use crate::analysis::Cfg;
use crate::ir::{
    Block, Builder, Edge, Entity, Exit, Function, Op, PatchpointFlags, State, Successor, Table,
    Type, Unary, Value, Variable, evaluate,
};

/// About the number of instructions that a copy takes, if it can be made.
fn cost_of(function: &Function, value: Value) -> Option<u32> {
    const EXIT: u32 = 2;
    const PATCHPOINT: u32 = 4;
    match *function.op(value) {
        _ if function.type_of(value) == Type::Tuple => None,
        Op::Patchpoint(patchpoint) => {
            let flags = function.patchpoint(patchpoint).flags;
            (!flags.contains(PatchpointFlags::UNIQUE)).then_some(PATCHPOINT)
        }
        Op::Check(..) | Op::Checked(..) => Some(EXIT),
        Op::Unary(Unary::Wrap | Unary::Widen(Sign::Unsigned) | Unary::Opaque, _) => Some(0),
        _ => Some(1),
    }
}

/// What a copy of some instructions would be like.
pub(super) struct Preview {
    /// Not counting the end of a block.
    pub(super) cost: u32,
    /// The edge that is certain to be taken, if the end of a block is included.
    pub(super) taken: Option<Edge>,
    /// Whether the last check that is certain to pass or to fail does the latter.
    pub(super) settled_check_fails: Option<bool>,
}

impl Preview {
    pub(super) const MOST_INSTRUCTIONS: usize = 8;

    /// `known` gives the bits of what is constant in the copy. `None` if they are many or cannot be copied.
    pub(super) fn of(
        function: &Function,
        first: Value,
        last: Value,
        known: impl Fn(Value) -> Option<u64>,
    ) -> Option<Self> {
        let mut folded = [(Value::NONE, 0); Self::MOST_INSTRUCTIONS];
        let mut preview = Self {
            cost: 0,
            taken: None,
            settled_check_fails: None,
        };
        let (mut value, block) = (first, function.block_of(first));
        for position in 0..Self::MOST_INSTRUCTIONS {
            let cost = cost_of(function, value)?;
            let of = |operand| {
                let mut earlier = folded[..position].iter();
                known(operand).or_else(|| Some(earlier.find(|found| found.0 == operand)?.1))
            };
            if value == function.last(block) {
                preview.taken = successor_taken(function, block, of);
            } else if let Op::Check(condition, _) = *function.op(value)
                && let Some(bits) = of(condition)
            {
                preview.settled_check_fails = Some(bits != 0);
                preview.cost += if bits == 0 { 0 } else { cost };
            } else if let Some(bits) = fold(function, value, of) {
                folded[position] = (value, bits);
            } else {
                preview.cost += cost;
            }
            if value == last {
                return Some(preview);
            }
            value = function.next(value);
        }
        None
    }
}

/// The edge, if its source only jumps, or else that of a new block in its middle.
pub(super) fn as_jump(function: &mut Function, edge: Edge) -> Edge {
    if *function.op(function.last(function.source(edge))) == Op::Jump {
        edge
    } else {
        let middle = function.split_edge(edge);
        function.successor(middle, 0)
    }
}

struct Copy {
    original: Value,
    block: Block,
    value: Value,
}

/// Works on one path at a time: a series of blocks, each the successor of the one before, that are copied to one block.
pub(super) struct Copier {
    /// What stands for the values on the path.
    stand_ins: Table<Value, Value>,
    on_path: Vec<Value>,
    last_state: (State, State),
    copies: Vec<Copy>,
    args: Vec<Value>,
}

#[derive(Clone, Copy)]
enum Use {
    Operand(Value),
    Arg(Edge),
    Kept(State),
}

impl Copier {
    /// For the values that exist now.
    pub(super) fn new(function: &Function) -> Self {
        Self {
            stand_ins: function.value_table(Value::NONE),
            on_path: Vec::new(),
            last_state: (State::NONE, State::NONE),
            copies: Vec::new(),
            args: Vec::new(),
        }
    }

    #[inline]
    fn stand_in(&self, value: Value) -> Value {
        stand_in(&self.stand_ins, value)
    }

    pub(super) fn set_stand_in(&mut self, original: Value, value: Value) {
        self.stand_ins[original] = value;
        self.on_path.push(original);
    }

    /// What stands for a parameter of the target of the edge, or for a value from another block.
    pub(super) fn stand_in_along(&self, function: &Function, edge: Edge, value: Value) -> Value {
        match *function.op(value) {
            Op::Param(position) if function.block_of(value) == function.target(edge) => {
                function.args(edge)[position as usize]
            }
            _ => self.stand_in(value),
        }
    }

    /// Of the target of the edge.
    pub(super) fn preview(&self, function: &Function, edge: Edge) -> Option<Preview> {
        let block = function.target(edge);
        let known = |value| function.constant_bits(self.stand_in_along(function, edge, value));
        Preview::of(function, function.first(block), function.last(block), known)
    }

    /// Replaces the jump along `edge` with all instructions of its target but the last.
    pub(super) fn copy_body(&mut self, function: &mut Function, edge: Edge) {
        let (into, from) = (function.source(edge), function.target(edge));
        for (&param, &arg) in function.params(from).iter().zip(function.args(edge)) {
            self.set_stand_in(param, arg);
        }
        function.remove(function.last(into));
        self.copy_body_from(function, function.first(from), into);
    }

    /// Up to the end of the block, which is not included.
    pub(super) fn copy_body_from(&mut self, function: &mut Function, first: Value, into: Block) {
        let (mut value, end) = (first, function.last(function.block_of(first)));
        while value != end {
            self.copy_instruction(function, value, into);
            value = function.next(value);
        }
    }

    /// Follows the body. `taken` is the edge that the block is certain to take.
    pub(super) fn copy_end(
        &mut self,
        function: &mut Function,
        from: Block,
        into: Block,
        taken: Option<Edge>,
    ) {
        let end = function.last(from);
        let Some(taken) = taken else {
            self.copy_instruction(function, end, into);
            function.clear_successors(into);
            for edge in function.successors(from) {
                self.copy_edge(function, edge, into);
            }
            return;
        };
        function.append(into, Op::Jump, Type::Void, function.origin(end));
        function.clear_successors(into);
        let edge = self.copy_edge(function, taken, into);
        function.set_rare(edge, false);
    }

    fn copy_edge(&mut self, function: &mut Function, edge: Edge, into: Block) -> Edge {
        self.args.clear();
        let args = function.args(edge).iter();
        self.args
            .extend(args.map(|&arg| stand_in(&self.stand_ins, arg)));
        let successor = Successor {
            target: function.target(edge),
            args: &self.args,
            is_rare: function.is_rare(edge),
        };
        function.push_successor(into, successor)
    }

    pub(super) fn end_path(&mut self, into: Block) {
        self.last_state = (State::NONE, State::NONE);
        for original in self.on_path.drain(..) {
            self.copies.push(Copy {
                original,
                block: into,
                value: core::mem::replace(&mut self.stand_ins[original], Value::NONE),
            });
        }
    }

    fn copy_instruction(&mut self, function: &mut Function, value: Value, into: Block) {
        let (ty, origin) = (function.type_of(value), function.origin(value));
        let state = self.copy_state(function, function.state_of(value));
        let mut op = function.clone_op(value);
        for operand in op.inline_operands_mut() {
            *operand = self.stand_in(*operand);
        }
        let mut builder = Builder::at_end(function, into);
        builder.origin = origin;
        let copy = match op {
            Op::Unary(op, a) => builder.convert(op, ty, a),
            Op::Binary(op, [a, b]) => builder.binary(op, a, b),
            Op::Compare(cond, [a, b]) => builder.compare(cond, a, b),
            Op::FloatCompare(cond, [a, b]) => builder.float_compare(cond, a, b),
            Op::Select([condition, then, or]) => builder.select(condition, then, or),
            Op::Check(condition, exit) => return builder.check(condition, Exit { state, ..exit }),
            Op::Checked(op, [a, b], exit) => builder.checked(op, a, b, Exit { state, ..exit }),
            Op::Patchpoint(_) | Op::Return(_) => {
                let copy = builder.insert(op, ty);
                for operand in function.operands_mut(copy) {
                    *operand = self.stand_in(*operand);
                }
                if !state.is_none() {
                    function.set_state_of(copy, state);
                }
                copy
            }
            _ => builder.insert(op, ty),
        };
        if ty != Type::Void {
            self.set_stand_in(value, copy);
        }
    }

    fn copy_state(&mut self, function: &mut Function, state: State) -> State {
        if state != self.last_state.0 {
            let values = function.state(state);
            let changes = values.iter().any(|&value| self.stand_in(value) != value);
            let copy = if changes {
                function.map_state(state, |value| stand_in(&self.stand_ins, value))
            } else {
                state
            };
            self.last_state = (state, copy);
        }
        self.last_state.1
    }

    pub(super) fn repair_uses(self, function: &mut Function, analyses: &mut Analyses) {
        analyses.invalidate();
        if self.copies.is_empty() {
            return;
        }
        let (cfg, dominators) = analyses.dominators(function);
        let mut reaching = Reaching::new(cfg, dominators);
        let mut variables = Table::filled(self.stand_ins.len(), Variable::NONE);
        for &Copy { original, .. } in &self.copies {
            if variables[original].is_none() {
                variables[original] = reaching.add_variable(function.type_of(original));
                let home = function.block_of(original);
                if cfg.is_reachable(home) {
                    reaching.define(variables[original], home, Held::Value(original));
                }
            }
        }
        for copy in &self.copies {
            if cfg.is_reachable(copy.block) {
                let held = match variable_of(&variables, copy.value) {
                    Some(other) if function.block_of(copy.value) != copy.block => {
                        Held::AnswerTo(reaching.read(other, copy.block))
                    }
                    _ => Held::Value(copy.value),
                };
                reaching.define(variables[copy.original], copy.block, held);
            }
        }
        let uses = find_uses(function, cfg, &variables, &mut reaching);
        reaching.solve(function);
        for (user, position, read) in uses {
            let values = match user {
                Use::Operand(value) => function.operands_mut(value),
                Use::Arg(edge) => function.args_mut(edge),
                Use::Kept(state) => function.state_mut(state),
            };
            values[position as usize] = reaching.answer(read);
        }
    }
}

fn fold(function: &Function, value: Value, of: impl Fn(Value) -> Option<u64>) -> Option<u64> {
    let ty = |value| function.type_of(value);
    match *function.op(value) {
        Op::Unary(Unary::Opaque, _) => None,
        Op::Unary(op, a) => evaluate::unary(op, ty(a), ty(value), of(a)?),
        Op::Binary(op, [a, b]) => evaluate::binary(op, ty(value), of(a)?, of(b)?),
        Op::Checked(op, [a, b], _) => evaluate::checked(op, ty(value), of(a)?, of(b)?),
        Op::Compare(cond, [a, b]) => Some(evaluate::compare(cond, ty(a), of(a)?, of(b)?).into()),
        Op::FloatCompare(cond, [a, b]) => {
            Some(evaluate::float_compare(cond, ty(a), of(a)?, of(b)?).into())
        }
        Op::Select([condition, then, or]) => of(if of(condition)? != 0 { then } else { or }),
        _ => None,
    }
}

/// `of` gives the bits of the values that are known.
fn successor_taken(
    function: &Function,
    block: Block,
    of: impl Fn(Value) -> Option<u64>,
) -> Option<Edge> {
    let position = match *function.op(function.last(block)) {
        Op::Jump => 0,
        Op::Branch(condition) => usize::from(of(condition)? == 0),
        Op::Switch(subject, cases) => {
            let bits = of(subject)?;
            let key = if function.type_of(subject) == Type::I32 {
                (bits as i32).into()
            } else {
                bits as i64
            };
            let keys = function.cases(cases);
            let found = keys.iter().position(|&other| other == key);
            found.unwrap_or(keys.len())
        }
        Op::JumpTable(index, cases) => *function.cases(cases).get(of(index)? as usize)? as usize,
        _ => return None,
    };
    Some(function.successor(block, position))
}

#[inline]
fn stand_in(stand_ins: &Table<Value, Value>, value: Value) -> Value {
    if value.index() < stand_ins.len() && !stand_ins[value].is_none() {
        stand_ins[value]
    } else {
        value
    }
}

#[inline]
fn variable_of(variables: &Table<Value, Variable>, value: Value) -> Option<Variable> {
    if value.index() < variables.len() {
        variables[value].expand()
    } else {
        None
    }
}

/// Those outside the block of the value. A state that has such values is used in one block only from now on.
fn find_uses(
    function: &mut Function,
    cfg: &Cfg,
    variables: &Table<Value, Variable>,
    reaching: &mut Reaching<'_>,
) -> Vec<(Use, u32, Read)> {
    let mut uses = Vec::new();
    let mut note = |function: &Function, block: Block, user: Use, values: &[Value]| {
        let mut found_any = false;
        for (position, &value) in (0..).zip(values) {
            if let Some(variable) = variable_of(variables, value) {
                found_any = true;
                if function.block_of(value) != block {
                    uses.push((user, position, reaching.read(variable, block)));
                }
            }
        }
        found_any
    };
    let mut last_users = Table::filled(function.state_count(), (Block::NONE, State::NONE));
    for &block in cfg.order() {
        let mut value = function.first(block);
        while !value.is_none() {
            note(
                function,
                block,
                Use::Operand(value),
                function.operands(value),
            );
            if let Some(state) = function.state_of(value).expand() {
                let (last_user, own) = &mut last_users[state];
                if own.is_none() {
                    *own = state;
                    if note(function, block, Use::Kept(state), function.state(state)) {
                        *last_user = block;
                    }
                } else if !last_user.is_none() {
                    if *last_user != block {
                        (*last_user, *own) = (block, function.map_state(state, |kept| kept));
                        note(function, block, Use::Kept(*own), function.state(*own));
                    }
                    function.set_state_of(value, *own);
                }
            }
            value = function.next(value);
        }
        for edge in function.successors(block) {
            note(function, block, Use::Arg(edge), function.args(edge));
        }
    }
    uses
}
