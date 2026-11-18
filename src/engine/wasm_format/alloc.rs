//! Allocation that fails with an error, for what grows with the module.

use std::collections::TryReserveError;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OutOfMemory;

impl From<TryReserveError> for OutOfMemory {
    fn from(_: TryReserveError) -> Self {
        Self
    }
}

pub(crate) trait TryPush<T> {
    fn try_push(&mut self, item: T) -> Result<(), OutOfMemory>;
}

impl<T> TryPush<T> for Vec<T> {
    #[inline]
    fn try_push(&mut self, item: T) -> Result<(), OutOfMemory> {
        if self.len() == self.capacity() {
            self.try_reserve(1)?;
        }
        self.push(item);
        Ok(())
    }
}

pub(crate) fn try_boxed<T>(
    items: impl ExactSizeIterator<Item = T>,
) -> Result<Box<[T]>, OutOfMemory> {
    let mut vec = Vec::new();
    vec.try_reserve_exact(items.len())?;
    vec.extend(items);
    Ok(vec.into_boxed_slice())
}
