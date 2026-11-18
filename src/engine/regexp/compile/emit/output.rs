//! The program while it is being written, and its layout.

use crate::Error;
use crate::compile::memory::{filled, push, reserve};
use crate::program::{
    Join, Loop, LoopId, Op, Room, Scan, Scope, ScopeId, ScopeKind, Slot, Switch, Target,
};

/// Operations name labels where they will have targets.
pub(super) type Label = Target;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Main,
    /// What runs after a failure.
    Cold,
}

#[derive(Clone, Copy)]
struct Bound {
    section: Section,
    index: u32,
    room: Room,
    scope: ScopeId,
}

pub(super) struct Output {
    main: Vec<Op>,
    cold: Vec<Op>,
    labels: Vec<Option<Bound>>,
    pub(super) scopes: Vec<Scope>,
    pub(super) loops: Vec<Loop>,
    pub(super) choice_targets: Vec<Label>,
    pub(super) switches: Vec<Switch>,
    pub(super) scans: Vec<Scan>,
    pub(super) cell_weights: Vec<u32>,
}

pub(super) const ROOT: ScopeId = ScopeId(0);

/// Stands for a scope that comes into being after the body of the loop has been written.
pub(super) fn between_turns_of(id: LoopId) -> ScopeId {
    ScopeId(!id.0)
}

impl Output {
    pub(super) fn new(expected_ops: usize) -> Result<Output, Error> {
        let mut output = Output {
            main: Vec::new(),
            cold: Vec::new(),
            labels: Vec::new(),
            scopes: Vec::new(),
            loops: Vec::new(),
            choice_targets: Vec::new(),
            switches: Vec::new(),
            scans: Vec::new(),
            cell_weights: Vec::new(),
        };
        reserve(&mut output.main, expected_ops)?;
        reserve(&mut output.cold, expected_ops / 2)?;
        reserve(&mut output.labels, expected_ops)?;
        let root = Scope {
            parent: ROOT,
            height: 0,
            kind: ScopeKind::Root,
        };
        push(&mut output.scopes, root)?;
        Ok(output)
    }

    #[inline]
    pub(super) fn op(&mut self, section: Section, op: Op) -> Result<(), Error> {
        match section {
            Section::Main => push(&mut self.main, op),
            Section::Cold => push(&mut self.cold, op),
        }
    }

    pub(super) fn main_length(&self) -> u32 {
        self.main.len() as u32
    }

    /// The scope comes into being after the operation at `index` has been written.
    pub(super) fn name_scope_of_choice(&mut self, index: u32, choice: Slot) {
        if let Op::Choose { slot, .. } = &mut self.main[index as usize] {
            *slot = choice;
        }
    }

    pub(super) fn label(&mut self) -> Result<Label, Error> {
        push(&mut self.labels, None)?;
        Ok(Target(self.labels.len() as u32 - 1))
    }

    pub(super) fn bind(&mut self, label: Label, section: Section, room: Room, scope: ScopeId) {
        let index = match section {
            Section::Main => self.main.len(),
            Section::Cold => self.cold.len(),
        };
        self.labels[label.index()] = Some(Bound {
            section,
            index: index as u32,
            room,
            scope,
        });
    }

    pub(super) fn height(&self, scope: ScopeId) -> u32 {
        self.scopes[scope.index()].height
    }

    /// The slot is that of the first cell.
    pub(super) fn add_scope(
        &mut self,
        parent: ScopeId,
        height: u32,
        kind: ScopeKind,
    ) -> Result<Slot, Error> {
        let missing = (height as usize).saturating_sub(self.cell_weights.len());
        reserve(&mut self.cell_weights, missing)?;
        self.cell_weights
            .resize(self.cell_weights.len() + missing, 0);
        let scope = Scope {
            parent,
            height,
            kind,
        };
        push(&mut self.scopes, scope)?;
        Ok(Slot {
            cell: self.height(parent),
            scope: ScopeId(self.scopes.len() as u32 - 1),
        })
    }

    /// The nearest scope from here up that an operation enters.
    pub(super) fn without_reservations(&self, mut scope: ScopeId) -> ScopeId {
        while self.scopes[scope.index()].kind == ScopeKind::Reserved {
            scope = self.scopes[scope.index()].parent;
        }
        scope
    }

    /// Puts the sections together without what cannot be reached, and turns the labels into targets.
    pub(super) fn lay_out(&mut self) -> Result<(Vec<Op>, Vec<Join>), Error> {
        let main_length = self.main.len() as u32;
        let mut ops = core::mem::take(&mut self.main);
        reserve(&mut ops, self.cold.len())?;
        ops.append(&mut self.cold);
        for bound in self.labels.iter_mut().flatten() {
            if bound.section == Section::Cold {
                bound.index += main_length;
            }
        }

        let (mut new_index, is_used) = self.reach(&mut ops)?;
        let mut kept = 0;
        for index in 0..ops.len() {
            let is_reached = new_index[index] != UNREACHED;
            new_index[index] = kept;
            if is_reached {
                ops[kept as usize] = ops[index];
                kept += 1;
            }
        }
        ops.truncate(kept as usize);

        let labels = &self.labels;
        let resolve = |label: &mut Label| {
            let bound = labels[label.index()].filter(|_| is_used[label.index()]);
            *label = Target(bound.map_or(UNREACHED, |bound| new_index[bound.index as usize]));
        };
        ops.iter_mut().filter_map(Op::target_mut).for_each(resolve);
        self.choice_targets.iter_mut().for_each(resolve);
        let loops = self.loops.iter_mut();
        loops.for_each(|it| resolve(&mut it.back_into_turn));
        let cases = self.switches.iter_mut().flat_map(|it| &mut it.cases);
        cases.for_each(|case| resolve(&mut case.target));

        let mut joins: Vec<Join> = Vec::new();
        reserve(&mut joins, is_used.iter().filter(|&&it| it).count())?;
        let used = labels.iter().zip(&is_used).filter(|(_, is_used)| **is_used);
        joins.extend(used.filter_map(|(bound, _)| {
            bound.map(|bound| Join {
                at: Target(new_index[bound.index as usize]),
                room: bound.room,
                scope: match self.loops.get(!bound.scope.0 as usize) {
                    Some(it) => it.between_turns,
                    None => bound.scope,
                },
            })
        }));
        joins.sort_unstable_by_key(|join| join.at);
        joins.dedup_by(|join, kept| {
            let is_same = join.at == kept.at;
            if is_same {
                kept.room = kept.room.min(join.room);
                kept.scope = kept.scope.min(join.scope);
            }
            is_same
        });
        Ok((ops, joins))
    }

    /// Marks the operations that can run and the labels that they go to.
    fn reach(&self, ops: &mut [Op]) -> Result<(Vec<u32>, Vec<bool>), Error> {
        let mut marks = filled(ops.len(), UNREACHED)?;
        let mut is_used = filled(self.labels.len(), false)?;
        let mut starts = Vec::new();
        reserve(&mut starts, self.labels.len() + 1)?;
        starts.push(0);
        let mut use_label = |label: Label, starts: &mut Vec<u32>| {
            if !core::mem::replace(&mut is_used[label.index()], true) {
                let bound = self.labels[label.index()].expect("a label that is used is bound");
                starts.push(bound.index);
            }
        };
        let of_loops = self.loops.iter().map(|it| it.back_into_turn);
        for label in of_loops.chain(self.choice_targets.iter().copied()) {
            use_label(label, &mut starts);
        }
        while let Some(start) = starts.pop() {
            for index in start as usize.. {
                if core::mem::replace(&mut marks[index], 0) != UNREACHED {
                    break;
                }
                let op = &mut ops[index];
                if let Some(&mut label) = op.target_mut() {
                    use_label(label, &mut starts);
                }
                if let Op::Switch { table, .. } = *op {
                    for case in self.switches[table.index()].cases() {
                        use_label(case.target, &mut starts);
                    }
                }
                if !op.falls_through() {
                    break;
                }
            }
        }
        Ok((marks, is_used))
    }
}

const UNREACHED: u32 = u32::MAX;
