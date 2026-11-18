//! Chooses the units to look at when searching for the start of a match.

use smallvec::SmallVec;

use super::Next;
use super::frequency::Frequencies;
use super::prefix::{Cell, MAX_COLUMNS, MAX_ROWS, Prefixes};
use crate::Error;
use crate::compile::character::set_of;
use crate::compile::graph::{Graph, Step, masked_units};
use crate::program::{ClassId, Probe, RowProbe, Scan, Width};
use crate::set::{CharRange, Latin1Set, Set};

// Costs are in cycles for sixteen positions.
const COST_OF_LOOP: f32 = 1.2;
const COST_OF_STOP: f32 = 16.0 * 24.0;
const COST_OF_LOW_HALVES: f32 = 0.8;
const COST_OF_BOTH_HALVES: f32 = 2.2;
const COST_OF_NARROWING: f32 = 1.6;

/// Above this the attempts themselves are the quickest search.
const MAX_STOPS: f32 = 0.3;
/// Neighbours in text are not independent of each other.
const LEAST_SHARE_OF_LATER_PROBE: f32 = 0.02;

type Columns<T> = SmallVec<[T; MAX_COLUMNS + 1]>;

/// What all rows allow at an offset.
struct Column {
    offset: i16,
    units: Next,
    /// All of them, if they are few.
    few: Option<SmallVec<[u16; 4]>>,
    /// The class, if it is one of the pattern.
    class: Option<ClassId>,
    share: f32,
    cost: f32,
}

pub(crate) fn plan_scan(
    graph: &mut Graph,
    prefixes: &Prefixes,
    frequencies: &Frequencies,
) -> Result<Option<Scan>, Error> {
    let planner = Planner {
        graph,
        prefixes,
        frequencies,
    };
    let columns = planner.columns();
    let (chosen, share, cost) = choose_probes(&columns);
    let rows = planner.choose_row_probes(&columns);
    let rows = rows.filter(|&(_, _, cost_of_rows)| cost_of_rows < cost);
    let share = rows.as_ref().map_or(share, |rows| rows.1);
    let Some((&first, others)) = chosen.split_first().filter(|_| share <= MAX_STOPS) else {
        return Ok(None);
    };
    let mut probe = |column: &Column| {
        Ok::<_, Error>(Probe {
            offset: column.offset,
            class: class_of(graph, column)?,
        })
    };
    let mut scan = Scan {
        need: 0,
        first: probe(&columns[first])?,
        others: SmallVec::new(),
        rows: rows.map_or_else(Vec::new, |rows| rows.0),
    };
    for &other in others.iter().filter(|_| scan.rows.is_empty()) {
        scan.others.push(probe(&columns[other])?);
    }
    scan.need = scan.offsets().max().map_or(0, |last| last + 1) as u32;
    Ok(Some(scan))
}

fn class_of(graph: &mut Graph, column: &Column) -> Result<ClassId, Error> {
    if let Some(class) = column.class {
        return Ok(class);
    }
    let set = if !column.units.has_wide() || graph.width == Width::Latin1 {
        Set::from_latin1(*column.units.latin1())
    } else if let Some(few) = &column.few {
        set_of(few.iter().map(|&unit| CharRange::single(unit.into())))?
    } else {
        set_of(column.units.ranges())?
    };
    graph.add_class(set)
}

/// Returns their indices, the share of the positions that pass them, and the cost.
fn choose_probes(columns: &[Column]) -> (SmallVec<[usize; Scan::MAX_PROBES]>, f32, f32) {
    let (mut chosen, mut share, mut fixed) = (SmallVec::new(), 1.0, COST_OF_LOOP);
    while chosen.len() < Scan::MAX_PROBES {
        let floor = if chosen.is_empty() { 0.0 } else { LEAST_SHARE_OF_LATER_PROBE };
        let after = |column: &Column| share * column.share.max(floor);
        let total = |column: &Column| column.cost + after(column) * COST_OF_STOP;
        let left = (0..columns.len()).filter(|index| !chosen.contains(index));
        let best = left.min_by(|&a, &b| total(&columns[a]).total_cmp(&total(&columns[b])));
        match best.filter(|&best| total(&columns[best]) < share * COST_OF_STOP) {
            Some(best) => {
                share = after(&columns[best]);
                fixed += columns[best].cost;
                chosen.push(best);
            }
            None => break,
        }
    }
    (chosen, share, fixed + share * COST_OF_STOP)
}

struct Planner<'a> {
    graph: &'a Graph,
    prefixes: &'a Prefixes,
    frequencies: &'a Frequencies,
}

impl Planner<'_> {
    fn cells_at(&self, offset: i16) -> impl Iterator<Item = Cell> {
        let rows = self.prefixes.rows.iter();
        rows.filter_map(move |row| match usize::try_from(offset) {
            Ok(column) => row.cells().get(column).copied(),
            Err(_) => row.before,
        })
    }

    fn columns(&self) -> Columns<Column> {
        let rows = &self.prefixes.rows;
        let length = rows.iter().map(|row| row.cells().len()).min().unwrap_or(0);
        let has_before = rows.iter().all(|row| row.before.is_some());
        let offsets = -i16::from(has_before)..length as i16;
        offsets.map(|offset| self.column(offset)).collect()
    }

    fn column(&self, offset: i16) -> Column {
        let mut units = Next::NOTHING;
        let mut few = Some(SmallVec::new());
        for cell in self.cells_at(offset) {
            units = units | self.prefixes.units(self.graph, cell);
            few = few.and_then(|few| self.with_members(few, cell));
        }
        let mut cells = self.cells_at(offset);
        let class = match cells.next() {
            Some(first @ Cell::Step(Step::Class(class))) if cells.all(|it| it == first) => Some(class),
            _ => None,
        };
        let share = match &few {
            Some(few) => few.iter().map(|&unit| self.frequencies.of_unit(unit)).sum(),
            None => self.share_of(&units),
        };
        Column {
            offset,
            cost: self.cost_of(&units, few.as_deref()),
            units,
            few,
            class,
            share,
        }
    }

    fn with_members(&self, mut few: SmallVec<[u16; 4]>, cell: Cell) -> Option<SmallVec<[u16; 4]>> {
        let mut add = |unit: u16| {
            if !few.contains(&unit) {
                (few.len() < few.inline_size()).then(|| few.push(unit))?;
            }
            Some(())
        };
        match cell {
            Cell::Step(Step::Unit { value, mask }) => masked_units(value, mask).try_for_each(add)?,
            Cell::Step(Step::Class(class)) => {
                let ranges = self.graph[class].ranges().take_while(|it| it.first <= 0xffff);
                let mut members = ranges.flat_map(|it| it.first..=it.last.min(0xffff));
                members.try_for_each(|c| add(c as u16))?;
            }
            Cell::Union(_) => {
                let units = self.prefixes.units(self.graph, cell);
                if units.has_wide() && self.graph.width == Width::Utf16 {
                    return None;
                }
                units.latin1().iter().try_for_each(|c| add(c.into()))?;
            }
        }
        Some(few)
    }

    fn share_of(&self, units: &Next) -> f32 {
        let latin1 = self.frequencies.of_latin1(units.latin1());
        if self.graph.width == Width::Latin1 {
            return latin1;
        }
        let wide = units.wide_ranges().map(|it| self.frequencies.of_wide(it));
        latin1 + wide.sum::<f32>()
    }

    fn cost_of(&self, units: &Next, few: Option<&[u16]>) -> f32 {
        let compared = match few {
            Some([_]) => Some(0.6),
            Some(&[a, b]) if (a ^ b).is_power_of_two() => Some(0.9),
            Some(few @ ([_, _] | [_, _, _])) => Some(0.6 * few.len() as f32),
            _ if units.ranges().nth(1).is_none() => Some(1.1),
            _ => None,
        };
        match (compared, self.graph.width) {
            (Some(cost), width) => cost * width.bytes_per_unit() as f32,
            (None, Width::Latin1) => COST_OF_BOTH_HALVES,
            (None, Width::Utf16) => COST_OF_BOTH_HALVES + COST_OF_NARROWING,
        }
    }

    /// The members as a row probe sees them.
    fn bytes(&self, cell: Cell) -> Latin1Set {
        let units = self.prefixes.units(self.graph, cell);
        let mut bytes = *units.latin1();
        if units.has_wide() && self.graph.width == Width::Utf16 {
            bytes.insert(0xff);
        }
        bytes
    }

    /// Returns them, the share of the positions that pass them, and the cost.
    fn choose_row_probes(&self, columns: &[Column]) -> Option<(Vec<RowProbe>, f32, f32)> {
        let rows = self.prefixes.rows.len();
        if rows < 2 || columns.len() < 2 {
            return None;
        }
        let by_low_half: [f32; 16] = core::array::from_fn(|low| {
            let characters = (0..8).map(|high| high << 4 | low as u16);
            characters.map(|c| self.frequencies.of_unit(c)).sum()
        });
        let narrowing = match self.graph.width {
            Width::Latin1 => 0.0,
            Width::Utf16 => COST_OF_NARROWING,
        };
        let mut candidates: Vec<Candidate> = Vec::new();
        candidates.try_reserve_exact(columns.len() * 2).ok()?;
        for column in columns {
            let mut exact = Candidate::new(column.offset, COST_OF_BOTH_HALVES + narrowing);
            let mut coarse = Candidate::new(column.offset, COST_OF_LOW_HALVES + narrowing);
            for (row, cell) in self.cells_at(column.offset).enumerate() {
                let bytes = self.bytes(cell);
                let (low, high) = halves(&bytes);
                for half in 0..16 {
                    exact.probe.low[half] |= (low >> half & 1) as u8 * (1 << row);
                    exact.probe.high[half] |= (high >> half & 1) as u8 * (1 << row);
                    coarse.shares[row] += f32::from(low >> half & 1) * by_low_half[half];
                }
                exact.shares[row] = self.frequencies.of_latin1(&bytes);
            }
            coarse.probe.low = exact.probe.low;
            let is_ascii = exact.probe.high[8..] == [0; 8];
            coarse.probe.high[..if is_ascii { 8 } else { 16 }].fill(u8::MAX);
            candidates.extend([exact, coarse]);
        }

        let mut shares = [0.0; MAX_ROWS];
        shares[..rows].fill(1.0);
        let (mut chosen, mut fixed) = (Vec::<RowProbe>::new(), COST_OF_LOOP);
        let sum = |shares: &[f32; MAX_ROWS]| shares.iter().sum::<f32>();
        while chosen.len() < Scan::MAX_ROW_PROBES {
            let total = |it: &Candidate| it.cost + sum(&it.after(&shares)) * COST_OF_STOP;
            let is_left = |it: &&Candidate| chosen.iter().all(|taken| taken.offset != it.probe.offset);
            let left = candidates.iter().filter(is_left);
            let best = left.min_by(|a, b| total(a).total_cmp(&total(b)));
            match best.filter(|&best| total(best) < sum(&shares) * COST_OF_STOP) {
                Some(best) => {
                    shares = best.after(&shares);
                    fixed += best.cost;
                    chosen.push(best.probe);
                }
                None => break,
            }
        }
        Some((chosen, sum(&shares), fixed + sum(&shares) * COST_OF_STOP))
    }
}

struct Candidate {
    probe: RowProbe,
    shares: [f32; MAX_ROWS],
    cost: f32,
}

impl Candidate {
    fn new(offset: i16, cost: f32) -> Candidate {
        Candidate {
            probe: RowProbe {
                offset,
                low: [0; 16],
                high: [0; 16],
            },
            shares: [0.0; MAX_ROWS],
            cost,
        }
    }

    fn after(&self, shares: &[f32; MAX_ROWS]) -> [f32; MAX_ROWS] {
        core::array::from_fn(|row| shares[row] * self.shares[row].max(LEAST_SHARE_OF_LATER_PROBE / 4.0))
    }
}

/// The low and the high four bits that the members have.
fn halves(set: &Latin1Set) -> (u16, u16) {
    let (mut low, mut high) = (0, 0);
    for (index, word) in set.words().iter().enumerate() {
        for part in 0..4 {
            let row = (word >> (16 * part)) as u16;
            low |= row;
            high |= u16::from(row != 0) << (index * 4 + part);
        }
    }
    (low, high)
}
