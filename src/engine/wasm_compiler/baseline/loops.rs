//! Which locals a loop keeps in registers from one turn to the next.

use core::cmp::Reverse;
use core::ops::Range;

use bun_engine_asm::Bank;
use bun_engine_asm::prelude::*;
use bun_engine_wasm_format::{
    BlockType, BrTable, Catches, DataIndex, ElemIndex, Flow, FuncIndex, FunctionBody,
    FunctionFlags, MemoryIndex, RefType, TableIndex, TagIndex, TypeIndex, Visit, VisitControl,
    VisitMemory, VisitNumeric, VisitReference, VisitVariable, VisitVector,
};

use super::Function;
use super::cold::Cold;
use super::control::BlockKind;
use super::operand::{Operand, bank};
use super::registers::{Local, Registers};
use crate::abi::SLOT;

/// How many registers of each bank are left for operands and for the other locals.
const UNPINNED: u32 = 4;
/// How much more a use counts for being in an inner loop.
const INNER_WEIGHT: u32 = 2;
/// How many of the locals of a loop count for the loop around it.
const MOST_PASSED_ON: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Read,
    Assigned,
    /// Before anything can read it.
    AssignedFirst,
}

#[derive(Clone, Copy)]
struct Use {
    local: u32,
    weight: u32,
    role: Role,
    /// What `latest` had for the local before.
    outer: u32,
}

#[derive(Clone, Default)]
struct Plan {
    /// In `ranked`.
    locals: Range<u32>,
    calls: u32,
    branches: u32,
}

struct OpenLoop {
    index: u32,
    first_use: u32,
    depth: u32,
    calls: u32,
    branches: u32,
    is_entered_first: bool,
}

/// What a look ahead has found out about a loop and those in it.
#[derive(Default)]
pub(super) struct LoopPlans {
    plans: Vec<Plan>,
    /// For each loop its locals, those that matter most first.
    ranked: Vec<Use>,
    /// Of the open loops.
    uses: Vec<Use>,
    /// By local: one more than its last position in `uses`, or zero.
    latest: Vec<u32>,
    open: Vec<OpenLoop>,
}

impl LoopPlans {
    pub(super) fn start(&mut self, body: &FunctionBody<'_>) {
        let facts = body.facts();
        self.plans.clear();
        self.ranked.clear();
        if facts.loop_count != 0 {
            (self.plans).resize(facts.loop_count as usize, Plan::default());
            if self.latest.len() < facts.local_count as usize {
                self.latest.resize(facts.local_count as usize, 0);
            }
        }
    }

    /// For a function that has registers for all its locals.
    pub(super) fn keep_all(&mut self, local_count: u32) {
        if self.plans.is_empty() {
            return;
        }
        self.ranked.extend((0..local_count).map(|local| Use {
            local,
            weight: 1,
            role: Role::Assigned,
            outer: 0,
        }));
        self.plans.fill(Plan {
            locals: 0..local_count,
            calls: 0,
            branches: 0,
        });
    }

    #[inline(never)]
    fn scan(&mut self, body: &FunctionBody<'_>, offset: u32, first_index: u32) {
        let catches = FunctionFlags::HAS_TRY | FunctionFlags::HAS_TRY_TABLE;
        let mut scan = Scan {
            plans: self,
            first_index,
            first_use: 0,
            depth: 0,
            calls: 0,
            branches: 0,
            is_straight: false,
            calls_may_branch: body.facts().flags.intersects(catches),
        };
        let result = body.driver_at(offset).run(&mut scan);
        debug_assert!(result.is_err());
    }
}

struct EndOfLoop;

type Scanned = Result<(), EndOfLoop>;

struct Scan<'a> {
    plans: &'a mut LoopPlans,
    first_index: u32,
    /// These are about the innermost loop.
    first_use: u32,
    depth: u32,
    calls: u32,
    branches: u32,
    /// Whether every turn of it gets here, with nothing that leads elsewhere before.
    is_straight: bool,
    calls_may_branch: bool,
}

impl Scan<'_> {
    #[inline(always)]
    fn note(&mut self, local: u32, weight: u32, role: Role) -> Scanned {
        let LoopPlans { uses, latest, .. } = self.plans;
        let latest = &mut latest[local as usize];
        if *latest > self.first_use {
            let known = &mut uses[*latest as usize - 1];
            known.weight = known.weight.saturating_add(weight);
            if known.role == Role::Read && role != Role::Read {
                known.role = Role::Assigned;
            }
        } else {
            uses.push(Use {
                local,
                weight,
                role,
                outer: *latest,
            });
            *latest = uses.len() as u32;
        }
        Ok(())
    }

    #[inline(always)]
    fn assign(&mut self, local: u32) -> Scanned {
        let role = if self.is_straight {
            Role::AssignedFirst
        } else {
            Role::Assigned
        };
        self.note(local, 1, role)
    }

    #[inline(always)]
    fn open(&mut self) -> Scanned {
        self.depth += 1;
        Ok(())
    }

    #[inline(always)]
    fn branch(&mut self) -> Scanned {
        self.is_straight = false;
        self.branches += 1;
        Ok(())
    }

    #[inline(always)]
    fn call_operation(&mut self) -> Scanned {
        self.calls += 1;
        Ok(())
    }

    #[inline(always)]
    fn call_function(&mut self) -> Scanned {
        self.is_straight &= !self.calls_may_branch;
        self.call_operation()
    }

    #[inline(always)]
    fn close(&mut self) -> Scanned {
        if self.depth == 0 {
            return self.close_loop();
        }
        self.depth -= 1;
        Ok(())
    }

    #[inline(never)]
    fn close_loop(&mut self) -> Scanned {
        let LoopPlans {
            plans,
            ranked,
            uses,
            latest,
            open,
        } = self.plans;
        let outer = open.pop().expect("an open loop");
        let start = ranked.len();
        for own in uses.drain(self.first_use as usize..) {
            latest[own.local as usize] = own.outer;
            ranked.push(own);
        }
        ranked[start..].sort_unstable_by_key(|own| {
            (
                own.role == Role::AssignedFirst,
                Reverse(own.weight),
                own.local,
            )
        });
        ranked.truncate(start + MOST_PASSED_ON.min(ranked.len() - start));
        let end = ranked.len();
        plans[outer.index as usize] = Plan {
            locals: start as u32..end as u32,
            calls: self.calls,
            branches: self.branches,
        };
        if open.is_empty() {
            return Err(EndOfLoop);
        }

        self.first_use = outer.first_use;
        self.depth = outer.depth;
        self.calls = (outer.calls).saturating_add(self.calls.saturating_mul(INNER_WEIGHT));
        self.branches += outer.branches;
        self.is_straight = false;
        for index in start..end {
            let inner = self.plans.ranked[index];
            let role = match inner.role {
                Role::AssignedFirst if !outer.is_entered_first => Role::Assigned,
                role => role,
            };
            self.note(inner.local, inner.weight.saturating_mul(INNER_WEIGHT), role)?;
        }
        Ok(())
    }
}

impl Visit for Scan<'_> {
    type Error = EndOfLoop;

    #[inline(always)]
    fn unhandled(&mut self) -> Scanned {
        Ok(())
    }
}

macro_rules! all {
    ($action:ident: $($name:ident($($type:ty),*);)*) => {
        $(#[inline(always)]
        fn $name(&mut self, $(_: $type),*) -> Scanned {
            self.$action()
        })*
    };
}

impl VisitControl for Scan<'_> {
    #[inline(never)]
    fn loop_(&mut self, _: BlockType, index: u32) -> Scanned {
        self.plans.open.push(OpenLoop {
            index: self.first_index + index,
            first_use: self.first_use,
            depth: self.depth,
            calls: self.calls,
            branches: self.branches,
            is_entered_first: self.is_straight,
        });
        self.first_use = self.plans.uses.len() as u32;
        (self.depth, self.calls, self.branches) = (0, 0, 0);
        self.is_straight = true;
        Ok(())
    }

    #[inline(always)]
    fn if_(&mut self, _: BlockType) -> Scanned {
        self.branch()?;
        self.open()
    }

    all! { open:
        block(BlockType);
        try_(BlockType);
        try_table(BlockType, Catches<'_>);
    }

    all! { close:
        end(Flow);
        delegate(u32, Flow);
    }

    all! { branch:
        unreachable();
        br(u32);
        br_if(u32);
        br_table(BrTable<'_>);
        br_on_null(u32);
        br_on_non_null(u32);
        br_on_cast(u32, RefType, RefType);
        br_on_cast_fail(u32, RefType, RefType);
        return_();
        return_call(FuncIndex, u32);
        return_call_indirect(TypeIndex, TableIndex, u32);
        return_call_ref(TypeIndex, u32);
        throw(TagIndex);
        throw_ref();
        rethrow(u32);
        catch(TagIndex, Flow);
        catch_all(Flow);
    }

    all! { call_function:
        call(FuncIndex, u32);
        call_indirect(TypeIndex, TableIndex, u32);
        call_ref(TypeIndex, u32);
    }
}

impl VisitVariable for Scan<'_> {
    #[inline(always)]
    fn local_get(&mut self, local: u32) -> Scanned {
        self.note(local, 1, Role::Read)
    }

    #[inline(always)]
    fn local_set(&mut self, local: u32) -> Scanned {
        self.assign(local)
    }

    #[inline(always)]
    fn local_tee(&mut self, local: u32) -> Scanned {
        self.assign(local)
    }
}

impl VisitMemory for Scan<'_> {
    all! { call_operation:
        memory_grow(MemoryIndex);
        memory_fill(MemoryIndex);
        memory_copy(MemoryIndex, MemoryIndex);
        memory_init(MemoryIndex, DataIndex);
        table_set(TableIndex);
    }
}

impl VisitReference for Scan<'_> {
    all! { call_operation:
        struct_new(TypeIndex);
        struct_new_default(TypeIndex);
        array_new(TypeIndex);
        array_new_default(TypeIndex);
        array_new_fixed(TypeIndex, u32);
        array_new_data(TypeIndex, DataIndex);
        array_new_elem(TypeIndex, ElemIndex);
        array_fill(TypeIndex);
        array_copy(TypeIndex, TypeIndex);
    }
}

impl VisitNumeric for Scan<'_> {}
impl VisitVector for Scan<'_> {}

impl Function<'_> {
    /// How many locals of the bank can stay in registers.
    pub(super) fn pin_limit(&self, bank: Bank) -> u32 {
        self.registers.allocatable.in_bank(bank).len() - UNPINNED
    }

    pub(super) fn enter_loop(&mut self, ty: BlockType, index: u32) {
        if self.loop_depth == 0 && !self.keeps_all_locals {
            self.loops.scan(&self.body, self.offset as u32, index);
        }
        self.loop_depth += 1;
        self.settle(self.height());
        self.pin_locals(index);

        let handler = self.innermost().handler;
        self.open(BlockKind::Loop(index), ty, handler);
        let header = self.new_target(None);
        self.innermost().target = Some(header);
        self.masm.bind(header.label);
        let mut resume = header.label;
        if let Some(counters) = self.counters {
            let entry = self.masm.new_label();
            let countdown = Absolute(counters.loop_countdowns + index as usize * 4);
            (self.masm).branch_add32_to_memory(ResultCond::NonNegative, countdown, 1, entry);
            resume = self.masm.here();
            self.cold.push(Cold::TierUpInLoop {
                entry,
                resume,
                index,
                snapshot: header.snapshot,
                dirty: header.dirty,
            });
        }
        if self.loop_entry == Some(index) {
            self.cold.push(Cold::TakeOverFrame {
                entry: self.entry,
                resume,
                snapshot: header.snapshot,
                held: header.held,
            });
        }
    }

    pub(super) fn leave_loop(&mut self, flow: Flow) {
        self.blocks.pop();
        self.loop_depth -= 1;
        let outer = *self.innermost();
        (self.registers.pinned, self.registers.unsaved) = (outer.pinned, outer.unsaved);
        self.is_dead = flow == Flow::Dead;
    }

    fn pin_locals(&mut self, index: u32) {
        let plan = self.loops.plans[index as usize].clone();
        let (start, end) = (plan.locals.start as usize, plan.locals.end as usize);
        let mut left = [self.pin_limit(Bank::Gpr), self.pin_limit(Bank::Fpr)];

        // One bit for each local of the plan.
        let mut chosen = 0u64;
        let mut staying = RegisterSet::EMPTY;
        for position in start..end {
            let own = self.loops.ranked[position];
            let Local { ty, register, .. } = self.locals[own.local as usize];
            let is_dead = own.role == Role::AssignedFirst;
            if is_dead {
                self.locals[own.local as usize].dead_at_loop = index;
                if let Some(reg) = register {
                    self.unbind(reg);
                }
            }
            let left = &mut left[bank(ty.kind()) as usize];
            if *left == 0 || own.weight <= plan.calls || is_dead && plan.branches <= 1 {
                continue;
            }
            *left -= 1;
            match register {
                Some(reg) if !is_dead && reg != SLOT.into() => staying.insert(reg),
                _ => {}
            }
            chosen |= 1 << (position - start);
        }
        if plan.calls == 0 {
            for reg in (self.registers.cached & self.registers.pinned) - staying {
                let left = &mut left[reg.bank() as usize];
                if *left != 0 && reg != SLOT.into() {
                    *left -= 1;
                    staying.insert(reg);
                }
            }
        }
        let leaving = self.registers.cached - staying;
        self.write_back_all(self.registers.dirty & leaving);
        self.unbind_all(leaving);
        if plan.calls != 0 {
            self.write_back_all(self.registers.dirty);
        }

        let mut pinned = staying;
        let mut assigned = RegisterSet::EMPTY;
        for position in (start..end).filter(|position| chosen >> (position - start) & 1 != 0) {
            let own = self.loops.ranked[position];
            let local = self.locals[own.local as usize];
            let reg = local.register.unwrap_or_else(|| {
                let mut vacant = (self.registers.free - pinned).in_bank(bank(local.ty.kind()));
                vacant.remove(SLOT);
                let reg = (local.hint.filter(|&hint| vacant.contains(hint)))
                    .or_else(|| Registers::best_for_local(vacant))
                    .expect("fewer are pinned than there are");
                if own.role != Role::AssignedFirst {
                    self.move_operand(reg, Operand::in_slot(local.ty, local.offset));
                }
                self.bind(own.local, reg);
                if own.role == Role::AssignedFirst {
                    self.unbind(reg);
                }
                reg
            });
            pinned.insert(reg);
            if own.role == Role::Assigned {
                assigned.insert(reg);
            }
        }
        self.registers.pinned = pinned;
        self.registers.unsaved = RegisterSet::EMPTY;
        if plan.calls == 0 {
            self.registers.unsaved = pinned;
            self.registers.dirty |= assigned;
        }
    }
}
