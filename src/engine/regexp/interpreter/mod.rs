//! Runs a program one operation at a time.

mod subject;

use bun_engine_text::{CodeUnit, Text};

use crate::context::{Context, FUEL, Meter, Outcome, Words};
use crate::program::{Direction, Loop, Op, Place, Program, UNSET, Width, join_halves};
use subject::Subject;

impl Program {
    /// Looks for a match from `start` on. If the program reports captures, `captures` has
    /// [`capture_entries`](Program::capture_entries) elements, and a match writes them all.
    pub fn exec(
        &self,
        subject: Text<'_>,
        start: u32,
        captures: &mut [u32],
        context: &mut Context,
    ) -> Outcome {
        assert!(subject.is_latin1() == (self.width == Width::Latin1));
        if start as usize > subject.len() {
            return Outcome::NoMatch;
        }
        let own_entries = match self.reports_captures {
            true => 0,
            false => self.capture_entries(),
        };
        let words = self.cell_count() + own_entries;
        if context.cells.ensure(words, usize::MAX).is_none() {
            return Outcome::OutOfMemory;
        }
        let (cells, own_captures) =
            context.cells.as_mut_slice()[..words].split_at_mut(self.cell_count());
        let machine = Machine {
            cells,
            captures: match self.reports_captures {
                true => &mut captures[..self.capture_entries()],
                false => own_captures,
            },
            stack: &mut context.stack,
            max_stack_words: context.max_stack_words,
            top: 0,
        };
        let (start, meter) = (start as usize, &mut context.meter);
        match subject {
            Text::Latin1(units) => machine.run(self, Subject::new(units), start, meter),
            Text::Utf16(units) => machine.run(self, Subject::new(units), start, meter),
        }
    }
}

struct Machine<'a> {
    cells: &'a mut [u32],
    captures: &'a mut [u32],
    stack: &'a mut Words,
    max_stack_words: usize,
    top: usize,
}

#[inline(always)]
fn plus(position: usize, offset: impl Into<i32>) -> usize {
    position.wrapping_add_signed(offset.into() as isize)
}

impl Machine<'_> {
    #[inline(always)]
    fn at(&self, place: Place) -> usize {
        plus(self.cells[place.slot.cell as usize] as usize, place.delta)
    }

    fn run<U: CodeUnit>(
        mut self,
        program: &Program,
        subject: Subject<'_, U>,
        start: usize,
        meter: &mut Meter,
    ) -> Outcome {
        let ops = program.ops();
        let (mut next, mut position, mut fuel) = (0, start, i64::from(FUEL));

        macro_rules! check {
            ($holds:expr, $fail:expr) => {
                if !$holds {
                    next = $fail.index();
                }
            };
        }
        macro_rules! spend {
            ($work:expr) => {{
                fuel -= $work as i64 + 1;
                if fuel < 0 {
                    if !meter.poll((i64::from(FUEL) - fuel) as u64) {
                        return Outcome::Interrupted;
                    }
                    fuel = FUEL.into();
                }
            }};
        }

        let outcome = loop {
            let op = &ops[next];
            next += 1;
            match *op {
                Op::Jump { to } | Op::Turned { to, .. } => next = to.index(),
                Op::NoMatch => break Outcome::NoMatch,
                Op::Match { start, end } => {
                    break Outcome::Match {
                        start: self.at(start) as u32,
                        end: plus(position, end) as u32,
                    };
                }
                Op::MatchHere { start, end } => {
                    break Outcome::Match {
                        start: plus(position, start) as u32,
                        end: plus(position, end) as u32,
                    };
                }
                Op::Need {
                    direction,
                    count,
                    fail,
                } => check!(subject.room(position, direction) >= count as usize, fail),
                Op::AtEdge {
                    direction,
                    offset,
                    fail,
                } => check!(subject.room(plus(position, offset), direction) == 0, fail),
                Op::NotInPair { offset, fail } => {
                    check!(!subject.is_in_pair(plus(position, offset)), fail);
                }
                Op::Unit {
                    offset,
                    value,
                    mask,
                    fail,
                } => {
                    let unit = subject.unit(plus(position, offset));
                    check!(unit.is_some_and(|unit| unit | mask == value), fail);
                }
                Op::Word {
                    bytes,
                    offset,
                    value,
                    mask,
                    fail,
                } => {
                    let word = subject.word(plus(position, offset), bytes);
                    let (value, mask) = (join_halves(value), join_halves(mask));
                    check!(word.is_some_and(|word| word | mask == value), fail);
                }
                Op::Class {
                    offset,
                    class,
                    fail,
                } => check!(
                    subject.is_in(plus(position, offset), program.class(class)),
                    fail
                ),
                Op::ClassCount {
                    offset,
                    count,
                    class,
                    fail,
                } => {
                    let (from, class) = (plus(position, offset), program.class(class));
                    let end = subject.run(from, Direction::Forward, class, count, 0);
                    check!(end - from == count as usize, fail);
                }
                Op::Pair {
                    offset,
                    class,
                    fail,
                } => {
                    let pair = subject.pair(plus(position, offset));
                    check!(pair.is_some_and(|c| program.class(class).contains(c)), fail);
                }
                Op::Switch { offset, table } => {
                    let unit = subject.unit(plus(position, offset)).unwrap_or_default();
                    next = program.switch(table).target(unit).index();
                }
                Op::Advance { by } => position = plus(position, by),
                Op::LeavePair => position -= usize::from(subject.is_in_pair(position)),
                Op::SkipToTail { length } => {
                    position = position.max(subject.len().saturating_sub(length as usize));
                }
                Op::Scan { id, fail } => {
                    check!(subject.scan(&mut position, program.scan(id), program), fail);
                }
                Op::SaveStart { slot, fail } => match position {
                    0 => self.cells[slot.cell as usize] = 0,
                    _ => next = fail.index(),
                },
                Op::Save { slot } => self.cells[slot.cell as usize] = position as u32,
                Op::Load { from, metered } => {
                    let left = core::mem::replace(&mut position, self.at(from));
                    if metered {
                        spend!(left.abs_diff(position));
                    }
                }
                Op::ComparePlace {
                    when,
                    offset,
                    place,
                    to,
                } => {
                    let here = position as i64 + i64::from(offset);
                    let slot = self.cells[place.slot.cell as usize];
                    check!(
                        !when.holds(here, i64::from(slot) + i64::from(place.delta)),
                        to
                    );
                }
                Op::Run {
                    direction,
                    code_points,
                    class,
                    max,
                    reserve,
                    ..
                } => {
                    let class = program.class(class);
                    position = match code_points {
                        true => subject.run_code_points(position, direction, class, max, reserve),
                        false => subject.run(position, direction, class, max, reserve),
                    };
                }
                Op::GiveBack {
                    direction,
                    stride,
                    floor,
                    filter,
                    fail,
                } => {
                    let filter = filter.class().map(|class| program.class(class));
                    let (floor, left) = (self.at(floor), position);
                    let found = subject.give_back(&mut position, direction, stride, floor, filter);
                    fuel -= left.abs_diff(position) as i64;
                    check!(found, fail);
                }
                Op::CodePoint {
                    direction,
                    class,
                    reserve,
                    fail,
                } => {
                    let class = program.class(class);
                    let end = subject.run_code_points(position, direction, class, 1, reserve);
                    check!(end != position, fail);
                    position = end;
                }
                Op::BackReference {
                    direction,
                    ignore_case,
                    first,
                    count,
                    reserve,
                    fail,
                } => {
                    let pairs = program.referenced_pairs(first, count).iter();
                    let mut starts = pairs.map(|&pair| usize::from(pair) * 2);
                    if let Some(entry) = starts.find(|&entry| self.captures[entry] != UNSET) {
                        let captured =
                            self.captures[entry] as usize..self.captures[entry + 1] as usize;
                        let end = subject
                            .repeats(captured, position, direction, ignore_case, program)
                            .filter(|&end| subject.room(end, direction) >= reserve as usize);
                        match end {
                            Some(end) => position = end,
                            None => next = fail.index(),
                        }
                    }
                }
                Op::Capture { offset, entry } => {
                    self.captures[entry as usize] = plus(position, offset) as u32;
                }
                Op::CapturePlace { place, entry } => {
                    self.captures[entry as usize] = self.at(place) as u32;
                }
                Op::Clear { pairs } => self.captures[pairs.entries()].fill(UNSET),
                Op::SaveCaptures { slot, pairs } => {
                    let entries = &self.captures[pairs.entries()];
                    self.cells[slot.cell as usize..][..entries.len()].copy_from_slice(entries);
                }
                Op::RestoreCaptures { slot, pairs } => {
                    let entries = &mut self.captures[pairs.entries()];
                    entries.copy_from_slice(&self.cells[slot.cell as usize..][..entries.len()]);
                }
                Op::Zero { slot } => self.cells[slot.cell as usize] = 0,
                Op::Increment { slot } => self.cells[slot.cell as usize] += 1,
                Op::CompareNumber {
                    when,
                    slot,
                    value,
                    to,
                } => {
                    let number = self.cells[slot.cell as usize];
                    check!(!when.holds(number.into(), value.into()), to);
                }
                Op::Tick => spend!(0),
                Op::Choose { slot, target, .. } => self.cells[slot.cell as usize] = target.0,
                Op::Resume { slot } => next = self.cells[slot.cell as usize] as usize,
                Op::Enter { id } => {
                    self.cells[program.loops[id.index()].turns_cell() as usize] = 0;
                }
                Op::Push { id } => {
                    if self.push(&program.loops[id.index()], position).is_none() {
                        break Outcome::OutOfMemory;
                    }
                }
                Op::Pop { id } => {
                    let popped = self.pop(&program.loops[id.index()]);
                    let left = core::mem::replace(&mut position, popped);
                    spend!(left.abs_diff(position));
                }
                Op::Retreat { id, none } => {
                    let it = &program.loops[id.index()];
                    next = match self.cells[it.turns_cell() as usize] {
                        0 => none.index(),
                        _ => it.back_into_turn().index(),
                    };
                }
                Op::Mark { slot } => self.cells[slot.cell as usize] = self.top as u32,
                Op::Cut { slot } => self.top = self.cells[slot.cell as usize] as usize,
            }
        };
        let work = i64::from(FUEL) - fuel + position.abs_diff(start) as i64;
        meter.work = meter.work.saturating_add(work as u64);
        outcome
    }

    #[inline]
    fn push(&mut self, it: &Loop, position: usize) -> Option<()> {
        let turns = &mut self.cells[it.turns_cell() as usize];
        let is_short = *turns == 0 && it.first_frame_is_short();
        *turns += 1;
        let top = self.top + if is_short { 1 } else { it.frame_size() };
        self.stack.ensure(top, self.max_stack_words)?;
        let (last, frame) = self.stack.as_mut_slice()[self.top..top].split_last_mut()?;
        *last = position as u32;
        if !is_short {
            let cells = &self.cells[it.cells().start as usize..it.cells().end as usize];
            let (of_cells, of_captures) = frame.split_at_mut(cells.len());
            of_cells.copy_from_slice(cells);
            of_captures.copy_from_slice(&self.captures[it.pairs().entries()]);
        }
        self.top = top;
        Some(())
    }

    #[inline]
    fn pop(&mut self, it: &Loop) -> usize {
        let turns = &mut self.cells[it.turns_cell() as usize];
        *turns -= 1;
        let is_short = *turns == 0 && it.first_frame_is_short();
        let top = self.top - if is_short { 1 } else { it.frame_size() };
        let (frame, last) = self.stack.as_mut_slice()[top..self.top].split_at(self.top - top - 1);
        let captures = &mut self.captures[it.pairs().entries()];
        if is_short {
            captures.fill(UNSET);
        } else {
            let cells = &mut self.cells[it.cells().start as usize..it.cells().end as usize];
            let (of_cells, of_captures) = frame.split_at(cells.len());
            cells.copy_from_slice(of_cells);
            captures.copy_from_slice(of_captures);
        }
        self.top = top;
        last[0] as usize
    }
}
