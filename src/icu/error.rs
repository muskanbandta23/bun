/// A `UErrorCode` that is a failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Error(i32);

pub type Result<T> = core::result::Result<T, Error>;

impl Error {
    pub const ILLEGAL_ARGUMENT: Error = Error(1);
    pub const MISSING_RESOURCE: Error = Error(2);
    pub const MEMORY_ALLOCATION: Error = Error(7);
    pub const INDEX_OUT_OF_BOUNDS: Error = Error(8);
    pub const BUFFER_OVERFLOW: Error = Error(15);
    pub const UNSUPPORTED: Error = Error(16);
    pub const RESOURCE_TYPE_MISMATCH: Error = Error(17);
}

/// The `UErrorCode` that a call reads and writes. Below zero is a warning.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub(crate) struct Status(i32);

impl Status {
    #[inline]
    pub(crate) fn result(self) -> Result<()> {
        if self.0 > 0 {
            Err(Error(self.0))
        } else {
            Ok(())
        }
    }
}
