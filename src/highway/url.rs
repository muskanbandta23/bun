//! The scanner of `bun_url_whatwg`, over kernels in src/jsc/bindings/highway_byte_set.cpp.

unsafe extern "C" {
    fn highway_url_stops_32(text: *const u8, len: usize, at: usize, set: &PlainBytes) -> u64;
    fn highway_url_stops_64(text: *const u8, len: usize, at: usize, set: &PlainBytes) -> u64;
    safe fn highway_url_stops_of_words(low: u64, high: u64, set: &PlainBytes) -> u64;
}

type Kernel = unsafe extern "C" fn(*const u8, usize, usize, &PlainBytes) -> u64;

/// The bytes that a scan passes over, to be built at compile time. It stops at all others.
#[repr(C, align(16))]
pub struct PlainBytes {
    /// Byte `b` is plain if `rows[b >> 4] & columns[b & 15]` isn't zero, and isn't [`Self::INSIDE`] behind a separator.
    rows: [u8; 16],
    columns: [u8; 16],
}

impl PlainBytes {
    const INSIDE: u8 = 1;

    /// `!` to `~` without `stops`, and without `leading_stops` where they follow a `.` or a `/`.
    pub const fn new(stops: &[u8], leading_stops: &[u8]) -> PlainBytes {
        const fn has(mut list: &[u8], byte: u8) -> bool {
            while let [first, rest @ ..] = list {
                if *first == byte {
                    return true;
                }
                list = rest;
            }
            false
        }

        let mut set = PlainBytes {
            rows: [0; 16],
            columns: [0; 16],
        };
        let mut byte = b'!';
        while byte <= b'~' {
            let (row, column) = ((byte >> 4) as usize, (byte & 15) as usize);
            set.rows[row] |= 1 << (row - 1);
            if has(leading_stops, byte) {
                set.rows[row] |= Self::INSIDE;
                set.columns[column] |= Self::INSIDE;
            } else if !has(stops, byte) {
                set.columns[column] |= 1 << (row - 1);
            }
            byte += 1;
        }

        // A stop cannot have the row of one leading stop and the column of another.
        let mut byte = b' ';
        while byte <= 0x7f {
            let is_stop = byte == b' ' || byte == 0x7f || has(stops, byte);
            assert!(set.is_stop(byte, 0) == is_stop);
            byte += 1;
        }
        set
    }

    const fn is_stop(&self, byte: u8, before: u8) -> bool {
        let shared = self.rows[(byte >> 4) as usize] & self.columns[(byte & 15) as usize];
        shared == 0 || (shared == Self::INSIDE && matches!(before, b'.' | b'/'))
    }

    fn scalar_stops(&self, text: &[u8], at: usize, count: usize) -> u64 {
        (at..text.len().min(at + count))
            .filter(|&index| {
                self.is_stop(text[index], if index == 0 { 0 } else { text[index - 1] })
            })
            .fold(0, |stops, index| stops | 1 << (index - at))
    }

    #[inline(always)]
    fn stops(&self, kernel: Kernel, text: &[u8], at: usize, count: usize) -> u64 {
        if at >= text.len() {
            return 0;
        }
        if cfg!(miri) {
            return self.scalar_stops(text, at, count);
        }
        let stops = if text.len() > 16 {
            // SAFETY: text.ptr/len are a readable range of at least 17 bytes, and `at` is inside it.
            unsafe { kernel(text.as_ptr(), text.len(), at, self) }
        } else {
            let (low, high) = words(text);
            (highway_url_stops_of_words(low, high, self) & ((1 << text.len()) - 1)) >> at
        };
        debug_assert_eq!(stops, self.scalar_stops(text, at, count));
        stops
    }

    /// A bit for each stop among the 32 bytes from `at` on, or as many as there are. The lowest bit is for `text[at]`.
    #[inline(always)]
    pub fn stops_in_32(&self, text: &[u8], at: usize) -> u64 {
        self.stops(highway_url_stops_32, text, at, 32)
    }

    /// The same for 64 bytes.
    #[inline(always)]
    pub fn stops_in_64(&self, text: &[u8], at: usize) -> u64 {
        self.stops(highway_url_stops_64, text, at, 64)
    }

    /// The indices of the stops from `at` on.
    #[inline(always)]
    pub fn scan<'a>(&'a self, text: &'a [u8], at: usize) -> Scan<'a> {
        Scan {
            set: self,
            text,
            end: at,
            stops: 0,
        }
    }

    /// A scan whose first step has been taken: `stops` is what [`Self::stops_in_64`] returned.
    #[inline(always)]
    pub fn scan_with<'a>(&'a self, text: &'a [u8], at: usize, stops: u64) -> Scan<'a> {
        Scan {
            set: self,
            text,
            end: at + 64,
            stops,
        }
    }

    /// The first block from `at` on that has a stop, or the last block: where it ends, and its stops.
    #[inline(never)]
    fn next_block(&self, text: &[u8], mut at: usize) -> (usize, u64) {
        loop {
            let stops = self.stops_in_64(text, at);
            at += 64;
            if stops != 0 || at >= text.len() {
                return (at, stops);
            }
        }
    }
}

/// Up to 16 bytes as two little-endian words, with zeros behind them.
#[inline(always)]
fn words(text: &[u8]) -> (u64, u64) {
    let len = text.len() as u32;
    if let (Some(first), Some(last)) = (text.first_chunk(), text.last_chunk()) {
        let high = u64::from_le_bytes(*last).unbounded_shr(8 * (16 - len));
        return (u64::from_le_bytes(*first), high);
    }
    if let (Some(first), Some(last)) = (text.first_chunk(), text.last_chunk()) {
        let [first, last] = [first, last].map(|bytes| u64::from(u32::from_le_bytes(*bytes)));
        return (first | last << (8 * (len - 4)), 0);
    }
    let low = text
        .iter()
        .rfold(0, |word, &byte| word << 8 | u64::from(byte));
    (low, 0)
}

pub struct Scan<'a> {
    set: &'a PlainBytes,
    text: &'a [u8],
    /// The end of the 64 bytes that `stops` is about.
    end: usize,
    stops: u64,
}

impl Scan<'_> {
    /// Passes over the stops before `at`, which is behind the last one returned.
    #[inline(always)]
    pub fn skip_to(&mut self, at: usize) {
        if at >= self.end {
            (self.end, self.stops) = (at, 0);
        } else {
            self.stops &= u64::MAX << (at + 64).saturating_sub(self.end);
        }
    }
}

impl Iterator for Scan<'_> {
    type Item = usize;

    #[inline(always)]
    fn next(&mut self) -> Option<usize> {
        if self.stops == 0 {
            if self.end >= self.text.len() {
                return None;
            }
            (self.end, self.stops) = self.set.next_block(self.text, self.end);
            if self.stops == 0 {
                return None;
            }
        }
        let at = self.end - 64 + self.stops.trailing_zeros() as usize;
        self.stops &= self.stops - 1;
        Some(at)
    }
}
