use crate::Value;

/// How the elements of an object are stored. Each of the dense kinds can represent what those before it can.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum ElementKind {
    None = 0x00,
    /// Only holes so far.
    Undecided = 0x02,
    /// Values that are int32, or empty.
    Int32 = 0x04,
    /// Raw doubles. NaN is a hole.
    Double = 0x06,
    Contiguous = 0x08,
    /// The first slot has a `SparseElements`.
    Sparse = 0x0a,
}

impl ElementKind {
    #[inline]
    pub fn for_value(value: Value) -> ElementKind {
        if value.is_int32() {
            ElementKind::Int32
        } else if value.as_double().is_some_and(|number| !number.is_nan()) {
            ElementKind::Double
        } else {
            ElementKind::Contiguous
        }
    }

    /// The least general kind that can represent what this one can, and `values`.
    #[inline]
    pub fn generalized_for(self, values: impl IntoIterator<Item = Value>) -> ElementKind {
        let mut kind = self;
        for value in values {
            if kind >= ElementKind::Contiguous {
                break;
            }
            kind = kind.max(ElementKind::for_value(value));
        }
        kind
    }

    /// The word for a hole.
    #[inline(always)]
    pub fn hole(self) -> u64 {
        if self == ElementKind::Double {
            Value::PURE_NAN_BITS
        } else {
            0
        }
    }

    #[inline(always)]
    pub fn is_dense(self) -> bool {
        self != ElementKind::None && self != ElementKind::Sparse
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(transparent)]
pub struct IndexingMode(u8);

impl IndexingMode {
    pub const IS_ARRAY: u8 = 0x01;
    pub const KIND_MASK: u8 = 0x0e;
    /// The storage is that of an `ImmutableArray`.
    pub const COPY_ON_WRITE: u8 = 0x10;
    /// A store into a hole or past the end has to ask the prototype chain, or whether the object is extensible.
    pub const SLOW_PUT: u8 = 0x20;
    /// A store may go straight to the storage if these bits are just the kind.
    pub const KIND_AND_WRITABILITY_MASK: u8 =
        Self::KIND_MASK | Self::COPY_ON_WRITE | Self::SLOW_PUT;

    pub const NON_ARRAY: IndexingMode = IndexingMode(0);

    #[inline(always)]
    pub const fn from_bits(bits: u8) -> IndexingMode {
        IndexingMode(bits)
    }

    #[inline(always)]
    pub const fn array(kind: ElementKind) -> IndexingMode {
        IndexingMode(Self::IS_ARRAY | kind as u8)
    }

    #[inline(always)]
    pub const fn non_array(kind: ElementKind) -> IndexingMode {
        IndexingMode(kind as u8)
    }

    #[inline(always)]
    pub const fn bits(self) -> u8 {
        self.0
    }

    #[inline(always)]
    pub const fn kind(self) -> ElementKind {
        let (bits, last) = (self.0 & Self::KIND_MASK, ElementKind::Sparse as u8);
        // SAFETY: each even number up to the last is a kind.
        unsafe { core::mem::transmute(if bits < last { bits } else { last }) }
    }

    #[inline(always)]
    pub const fn is_array(self) -> bool {
        self.0 & Self::IS_ARRAY != 0
    }

    #[inline(always)]
    pub const fn has_elements(self) -> bool {
        self.0 & Self::KIND_MASK != 0
    }

    #[inline(always)]
    pub const fn is_copy_on_write(self) -> bool {
        self.0 & Self::COPY_ON_WRITE != 0
    }

    #[inline(always)]
    pub const fn is_slow_put(self) -> bool {
        self.0 & Self::SLOW_PUT != 0
    }

    #[inline(always)]
    pub const fn with_kind(self, kind: ElementKind) -> IndexingMode {
        IndexingMode(self.0 & !(Self::KIND_MASK | Self::COPY_ON_WRITE) | kind as u8)
    }

    #[inline(always)]
    pub const fn copy_on_write(self) -> IndexingMode {
        IndexingMode(self.0 | Self::COPY_ON_WRITE)
    }

    #[inline(always)]
    pub const fn writable(self) -> IndexingMode {
        IndexingMode(self.0 & !Self::COPY_ON_WRITE)
    }

    #[inline(always)]
    pub const fn slow_put(self) -> IndexingMode {
        IndexingMode(self.0 | Self::SLOW_PUT)
    }
}
