//! The mutable state that an instruction keeps in its code block: caches and profiles.

use core::ops::Range;

macro_rules! metadata_kinds {
    ($($(#[$doc:meta])* $name:ident = $words:literal $($profiled:ident)?,)*) => {
        /// Entries of one kind are adjacent in the buffer, in the order of their instructions.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(u8)]
        pub enum MetadataKind {
            $($(#[$doc])* $name,)*
        }

        impl MetadataKind {
            pub const ALL: [MetadataKind; Self::COUNT] = [$(MetadataKind::$name),*];
            pub const COUNT: usize = [$($words),*].len();

            /// The size of an entry in words of 8 bytes.
            pub const fn words(self) -> u32 {
                match self {
                    $(MetadataKind::$name => $words,)*
                }
            }

            /// Whether the first word of an entry is the last value that the instruction produced.
            pub const fn has_value_profile(self) -> bool {
                match self {
                    $(MetadataKind::$name => metadata_kinds!(@profiled $($profiled)?),)*
                }
            }
        }
    };
    (@profiled profiled) => { true };
    (@profiled) => { false };
}

metadata_kinds! {
    Value = 1 profiled,
    UnaryArithmetic = 1,
    BinaryArithmetic = 1,
    /// Which ways the instruction has gone.
    Branch = 1,
    /// For an instruction that only compiled code caches.
    LazyCache = 1,
    LazyCacheAndValue = 2 profiled,
    GetById = 3 profiled,
    GetByIdDirect = 2 profiled,
    GetLength = 5 profiled,
    PutById = 3,
    GetByVal = 3 profiled,
    ArrayAccess = 2,
    GetPrivateName = 3 profiled,
    PutPrivateName = 3,
    PrivateBrand = 2,
    GetGlobal = 3 profiled,
    PutGlobal = 3,
    GetImport = 3 profiled,
    ResolveDynamic = 2,
    GetDynamic = 3 profiled,
    PutDynamic = 3,
    Call = 4 profiled,
    SuperConstruct = 5 profiled,
    ToThis = 2 profiled,
    /// The callee whose prototype the last object got.
    CachedCallee = 1,
    NewObject = 2,
    NewArray = 1,
    NewArrayWithSpecies = 4 profiled,
    SharedRegExp = 1,
    ForIn = 3,
    ForInGet = 4 profiled,
    IterationModes = 1,
    IteratorNext = 4 profiled,
    Catch = 1,
    Coverage = 1,
}

const _: () = assert!(MetadataKind::COUNT < 64);

/// How many entries of each kind a code block has.
#[derive(Clone, Copy)]
pub struct MetadataLayout<'a> {
    /// A bit for each kind that has entries.
    kinds: u64,
    is_wide: bool,
    /// For each of those, the word at which its entries end.
    ends: &'a [u8],
}

impl<'a> MetadataLayout<'a> {
    /// And the bytes after it.
    pub(crate) fn read(
        kinds: u64,
        is_wide: bool,
        bytes: &'a [u8],
    ) -> Option<(MetadataLayout<'a>, &'a [u8])> {
        let size = kinds.count_ones() as usize * if is_wide { 4 } else { 2 };
        let (ends, rest) = bytes.split_at_checked(size)?;
        let layout = MetadataLayout {
            kinds,
            is_wide,
            ends,
        };
        Some((layout, rest))
    }

    /// Where the first `present` kinds that have entries end.
    #[inline]
    fn end_of(self, present: u32) -> u32 {
        let Some(index) = (present as usize).checked_sub(1) else {
            return 0;
        };
        if self.is_wide {
            u32::from_le_bytes(self.ends.as_chunks().0[index])
        } else {
            u32::from(u16::from_le_bytes(self.ends.as_chunks().0[index]))
        }
    }

    pub fn words(self) -> u32 {
        self.end_of(self.kinds.count_ones())
    }

    /// The words that hold the entries of `kind`.
    pub fn range(self, kind: MetadataKind) -> Range<u32> {
        let bit = 1 << kind as u32;
        let below = self.kinds & (bit - 1);
        self.end_of(below.count_ones())..self.end_of((below | self.kinds & bit).count_ones())
    }

    pub fn count(self, kind: MetadataKind) -> u32 {
        let range = self.range(kind);
        (range.end - range.start) / kind.words()
    }

    /// The kinds that have entries, in the order of the buffer.
    pub fn ranges(self) -> impl Iterator<Item = (MetadataKind, Range<u32>)> + 'a {
        let (mut rest, mut present) = (self.kinds, 0);
        core::iter::from_fn(move || {
            let kind = *MetadataKind::ALL.get(rest.trailing_zeros() as usize)?;
            rest &= rest - 1;
            present += 1;
            Some((kind, self.end_of(present - 1)..self.end_of(present)))
        })
    }

    pub(crate) fn is_valid(self) -> bool {
        self.kinds >> MetadataKind::COUNT == 0
            && self.ranges().all(|(kind, range)| {
                range.start < range.end && (range.end - range.start) % kind.words() == 0
            })
    }
}
