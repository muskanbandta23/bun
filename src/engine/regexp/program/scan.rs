use smallvec::SmallVec;

use super::ClassId;

/// The unit at the offset is in the class.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Probe {
    pub offset: i16,
    pub class: ClassId,
}

/// Says which of eight ways to start a match allow the unit at the offset: `low[b & 15] & high[b >> 4]`, where `b` is
/// the unit, or 255 if that is less.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RowProbe {
    pub offset: i16,
    pub low: [u8; 16],
    pub high: [u8; 16],
}

impl RowProbe {
    #[inline]
    pub(crate) fn rows(&self, unit: u16) -> u8 {
        let byte = usize::from(unit.min(0xff));
        self.low[byte & 15] & self.high[byte >> 4]
    }
}

/// Finds the positions where a match can start. None starts where a probe fails, or where no row passes all its probes.
#[derive(Debug)]
pub struct Scan {
    /// The units that follow such a position, at least.
    pub(crate) need: u32,
    /// Holds where the scan stops. The others need not.
    pub(crate) first: Probe,
    pub(crate) others: SmallVec<[Probe; 3]>,
    pub(crate) rows: Vec<RowProbe>,
}

impl Scan {
    pub const MAX_PROBES: usize = 4;
    pub const MAX_ROW_PROBES: usize = 8;

    #[inline]
    pub fn need(&self) -> u32 {
        self.need
    }

    #[inline]
    pub fn first(&self) -> Probe {
        self.first
    }

    #[inline]
    pub fn others(&self) -> &[Probe] {
        &self.others
    }

    /// If there are any, they are better than the other probes.
    #[inline]
    pub fn rows(&self) -> &[RowProbe] {
        &self.rows
    }

    pub(crate) fn offsets(&self) -> impl Iterator<Item = i16> {
        let probes = core::iter::once(&self.first).chain(&self.others);
        let rows = self.rows.iter().map(|it| it.offset);
        probes.map(|it| it.offset).chain(rows)
    }
}
