use core::io::{BorrowedBuf, BorrowedCursor};
use core::mem::MaybeUninit;
use core::slice;

use bun_alloc::AllocError;

use crate::{CodeUnit, StringImpl, StringRef, Text, builder, copy, with_text};

/// Room for units, written from the front. A push that does not fit panics. A cursor is only ever lent, so whoever
/// lends it knows that what it has left behind is written.
pub struct Cursor<'a, T> {
    rest: &'a mut [MaybeUninit<T>],
}

impl<'a, T: CodeUnit> Cursor<'a, T> {
    /// Returns what `write` has written.
    #[inline(always)]
    pub fn fill_uninit(
        target: &mut [MaybeUninit<T>],
        write: impl FnOnce(&mut Cursor<'_, T>),
    ) -> &mut [T] {
        let capacity = target.len();
        let mut cursor = Cursor { rest: target };
        write(&mut cursor);
        let written = capacity - cursor.remaining();
        // SAFETY: the cursor has written what it has left behind.
        unsafe { target.get_unchecked_mut(..written).assume_init_mut() }
    }

    /// Returns how many units `write` has written.
    #[inline(always)]
    pub fn fill(target: &mut [T], write: impl FnOnce(&mut Cursor<'_, T>)) -> usize {
        // SAFETY: the layouts are the same, and a cursor writes only initialized units.
        let target = unsafe { &mut *(core::ptr::from_mut(target) as *mut [MaybeUninit<T>]) };
        Self::fill_uninit(target, write).len()
    }

    /// Appends what `write` writes, `additional` units at most.
    #[inline(always)]
    pub fn extend_vec(
        target: &mut Vec<T>,
        additional: usize,
        write: impl FnOnce(&mut Cursor<'_, T>),
    ) -> Result<(), AllocError> {
        target
            .try_reserve_exact(additional)
            .map_err(|_| AllocError)?;
        let written =
            Self::fill_uninit(&mut target.spare_capacity_mut()[..additional], write).len();
        // SAFETY: that many units behind the length are written.
        unsafe { target.set_len(target.len() + written) };
        Ok(())
    }

    #[inline(always)]
    pub fn remaining(&self) -> usize {
        self.rest.len()
    }

    /// The next `count` units, all of which the caller writes.
    #[inline(always)]
    fn take(&mut self, count: usize) -> &'a mut [MaybeUninit<T>] {
        let (taken, rest) = core::mem::take(&mut self.rest).split_at_mut(count);
        self.rest = rest;
        taken
    }

    /// The next `count` units, to be written in any order.
    ///
    /// # Safety
    /// The caller writes all of them, and only initialized units, before it gives the cursor back.
    #[inline(always)]
    pub unsafe fn take_uninit(&mut self, count: usize) -> &'a mut [MaybeUninit<T>] {
        self.take(count)
    }

    #[inline(always)]
    pub fn push(&mut self, unit: T) {
        self.take(1)[0].write(unit);
    }

    /// `code_point` is at most 0x10FFFF. Of a unit that is too wide, the low byte is kept.
    #[inline(always)]
    pub fn push_code_point(&mut self, code_point: u32) {
        match code_point.checked_sub(0x10000) {
            None => self.push(T::from_u16_truncating(code_point as u16)),
            Some(offset) => self.push_array([
                T::from_u16_truncating(0xd800 | (offset >> 10) as u16),
                T::from_u16_truncating(0xdc00 | (offset & 0x3ff) as u16),
            ]),
        }
    }

    #[inline(always)]
    pub fn push_array<const N: usize>(&mut self, units: [T; N]) {
        self.take(N).write_copy_of_slice(&units);
    }

    /// Takes room for all `N` units and keeps the first `count`: what follows may be overwritten.
    #[inline(always)]
    pub fn push_prefix<const N: usize>(&mut self, units: &[T; N], count: usize) {
        assert!(count <= N);
        self.rest[..N].write_copy_of_slice(units);
        self.take(count);
    }

    #[inline(always)]
    pub fn push_slice(&mut self, units: &[T]) {
        copy::copy(self.take(units.len()), units);
    }

    /// Of a unit that is too wide, the low byte is kept.
    #[inline(always)]
    pub fn push_text(&mut self, text: Text<'_>) {
        text.copy_to(self.take(text.len()));
    }

    /// `map` has no branch, or this is slow.
    #[inline(always)]
    pub fn push_mapped<S: Copy>(&mut self, source: &[S], map: impl Fn(S) -> T + Copy) {
        copy::map(self.take(source.len()), source, map);
    }

    /// [`Self::push_prefix`] of what `map` makes of `source`.
    #[inline(always)]
    pub(crate) fn push_mapped_prefix<S: Copy, const N: usize>(
        &mut self,
        source: &[S; N],
        count: usize,
        map: impl Fn(S) -> T + Copy,
    ) {
        assert!(count <= N);
        for (to, &from) in self.rest[..N].iter_mut().zip(source) {
            to.write(map(from));
        }
        self.take(count);
    }

    /// Returns false if a unit passes `stop`. Then nothing is pushed, but the room may be overwritten.
    #[inline(always)]
    pub(crate) fn push_mapped_unless<S: Copy>(
        &mut self,
        source: &[S],
        map: impl Fn(S) -> T + Copy,
        stop: impl Fn(S) -> bool + Copy,
    ) -> bool {
        if copy::map_unless(&mut self.rest[..source.len()], source, map, stop) {
            return false;
        }
        self.take(source.len());
        true
    }

    /// [`Self::push_mapped_unless`] of the units themselves, between quotation marks.
    #[inline(always)]
    pub(crate) fn push_quoted_unless<S: CodeUnit>(
        &mut self,
        source: &[S],
        stop: impl Fn(S) -> bool + Copy,
    ) -> bool {
        let [first, middle @ .., last] = &mut self.rest[..source.len() + 2] else {
            unreachable!()
        };
        first.write(T::from(b'"'));
        last.write(T::from(b'"'));
        if copy::map_unless(middle, source, |it| T::from_u16_truncating(it.to_u16()), stop) {
            return false;
        }
        self.take(source.len() + 2);
        true
    }

    #[inline(always)]
    pub fn push_repeated(&mut self, unit: T, count: usize) {
        self.take(count).write_filled(unit);
    }

    /// `length` units of `pattern`, which is not empty, again and again.
    #[inline]
    pub fn push_pattern(&mut self, pattern: Text<'_>, length: usize) {
        assert!(!pattern.is_empty());
        copy::fill(self.take(length), pattern);
    }

    /// Returns how many units of `source` it has read. See [`bun_highway::text::escape_json`].
    #[inline(always)]
    pub(crate) fn push_short_json_escapes<S: CodeUnit>(&mut self, source: &[S]) -> usize {
        let (read, written) = bun_highway::text::escape_json(source, self.rest);
        self.take(written);
        read
    }

    #[inline(always)]
    pub(crate) fn lend<R>(&mut self, write: impl FnOnce(BorrowedCursor<'_, T>) -> R) -> R {
        let mut buffer = BorrowedBuf::from(&mut *self.rest);
        let result = write(buffer.unfilled());
        let written = buffer.len();
        self.take(written);
        result
    }
}

impl Cursor<'_, u8> {
    /// Two bytes for each of `source`.
    #[inline(always)]
    pub(crate) fn push_mapped_pairs(&mut self, source: &[u8], map: impl Fn(u8) -> [u8; 2] + Copy) {
        let target = self.take(2 * source.len());
        // SAFETY: the same memory, and the alignment is one.
        let pairs = unsafe { slice::from_raw_parts_mut(target.as_mut_ptr().cast(), source.len()) };
        copy::map(pairs, source, map);
    }
}

impl StringRef {
    /// The string that `write` writes, of `capacity` units at most.
    #[inline(always)]
    pub fn try_write<T: CodeUnit>(
        capacity: usize,
        write: impl FnOnce(&mut Cursor<'_, T>),
    ) -> Result<StringRef, AllocError> {
        let string = StringImpl::allocate::<T>(capacity)?;
        // SAFETY: takes over the one count that the allocation was made with.
        let owner = unsafe { StringRef::from_raw(string) };
        // SAFETY: allocated for `T`, and nothing else has the string yet.
        let units = unsafe { &mut StringImpl::tail::<T>(string)[..capacity] };
        let length = Cursor::fill_uninit(units, write).len();
        if length == capacity && length != 0 {
            return Ok(owner);
        }
        // SAFETY: as above, and `length` units are written.
        Ok(unsafe { builder::shorten::<T>(owner, length) })
    }

    #[inline]
    pub(crate) fn try_copy(text: Text<'_>) -> Result<StringRef, AllocError> {
        with_text!(text, |units| Self::try_write(units.len(), |cursor| cursor
            .push_slice(units)))
    }
}
