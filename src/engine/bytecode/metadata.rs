//! The mutable state that an instruction keeps in its code block: caches and profiles.

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

/// How many entries of each kind a code block has.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MetadataLayout {
    /// Where the entries of each kind start, and where the last end.
    starts: [u32; MetadataKind::COUNT + 1],
}

impl MetadataLayout {
    /// `None` if the words do not fit in 32 bits.
    pub(crate) fn new(counts: &[u32; MetadataKind::COUNT]) -> Option<MetadataLayout> {
        let mut starts = [0u32; MetadataKind::COUNT + 1];
        for kind in MetadataKind::ALL {
            let words = counts[kind as usize].checked_mul(kind.words())?;
            starts[kind as usize + 1] = starts[kind as usize].checked_add(words)?;
        }
        Some(MetadataLayout { starts })
    }

    pub fn words(&self) -> u32 {
        self.starts[MetadataKind::COUNT]
    }

    /// The words that hold the entries of `kind`.
    pub fn range(&self, kind: MetadataKind) -> core::ops::Range<u32> {
        self.starts[kind as usize]..self.starts[kind as usize + 1]
    }

    pub fn count(&self, kind: MetadataKind) -> u32 {
        let range = self.range(kind);
        (range.end - range.start) / kind.words()
    }
}
