use core::mem::offset_of;

use bun_engine_bytecode::CloseMode;
use bun_engine_macros::Trace;
use bun_engine_text::{names, symbols};

use super::array_iterator::typed_array_length;
use super::protocol::{Caller, iterator_close_on_throw, iterator_close_quietly};
use super::string_iterator::code_point_string_at;
use crate::{
    CellType, Cx, ElementKind, Gc, HoleMode, IterationKind, IterationMode, JsArray,
    JsArrayBufferView, JsArrayIterator, JsCell, JsMap, JsMapIterator, JsObject, JsResult, JsSet,
    JsSetIterator, JsString, JsStringIterator, MapKind, OrderedCursor, RealmSlot, RealmWatchpoint,
    SetKind, TableKind, Thrown, Value, errors, iterator_complete, iterator_value,
    throw_not_iterable,
};

/// The Iterator Record of ECMA-262. Where nothing can tell the difference it has no iterator, or does not call its
/// `next`: see `IterationMode`. The three registers of the iteration instructions are one, the last at the lowest
/// address. It belongs to the realm in which it was made.
#[derive(Clone, Copy, Trace)]
#[repr(C)]
pub struct IteratorRecord {
    /// An int32 whose bits are unsigned. `DONE` in any mode, and otherwise zero in those that have no use for it.
    position: Value,
    next: Value,
    subject: Value,
}

fn is_slot(cx: &Cx, slot: RealmSlot, value: Value) -> bool {
    let cell = cx.global().slot_if_initialized(slot);
    cell.is_some_and(|cell| value == cell.into())
}

impl IteratorRecord {
    pub const OFFSET_OF_POSITION: usize = offset_of!(IteratorRecord, position);
    pub const OFFSET_OF_NEXT: usize = offset_of!(IteratorRecord, next);
    pub const OFFSET_OF_SUBJECT: usize = offset_of!(IteratorRecord, subject);
    pub const START: Value = Value::ZERO;
    /// As an index it is past the end of anything.
    pub const DONE: Value = Value::int32(-1);

    #[inline(always)]
    fn new(subject: Value, next: Value) -> IteratorRecord {
        IteratorRecord {
            position: Self::START,
            next,
            subject,
        }
    }

    /// `next` has been read from `iterator`.
    pub fn generic(iterator: Value, next: Value) -> IteratorRecord {
        assert!(!IterationMode::is_marker(next));
        Self::new(iterator, next)
    }

    /// For a cell that keeps the record in its fields.
    pub fn parts(self) -> [Value; 3] {
        [self.position, self.next, self.subject]
    }

    /// # Safety
    /// From `parts`.
    pub unsafe fn from_parts([position, next, subject]: [Value; 3]) -> IteratorRecord {
        IteratorRecord {
            position,
            next,
            subject,
        }
    }

    /// GetIterator(iterable, sync)
    #[inline]
    pub fn for_iterable(cx: &mut Cx, iterable: Value) -> JsResult<IteratorRecord> {
        Self::get_iterator(cx, iterable, Caller::Library)
    }

    /// GetIteratorFromMethod
    pub fn from_method(cx: &mut Cx, iterable: Value, method: Value) -> JsResult<IteratorRecord> {
        Self::get_iterator_from_method(cx, iterable, method, Caller::Library)
    }

    /// GetIteratorDirect
    pub fn direct(cx: &mut Cx, iterator: Value) -> JsResult<IteratorRecord> {
        match iterator.as_cell().and_then(|it| mode_of_iterator(cx, it)) {
            Some(mode) => Ok(Self::new(iterator, mode.marker())),
            None => Ok(Self::new(iterator, iterator.get(cx, names::next)?)),
        }
    }

    #[inline(always)]
    fn get_iterator(cx: &mut Cx, iterable: Value, caller: Caller) -> JsResult<IteratorRecord> {
        if let Some(cell) = iterable.as_cell()
            && cell.cell_type() == CellType::Array
            && cx.global().array_iterator_protocol_is_pristine()
            && cx.global().is_original_array_shape(&cell.shape())
        {
            return Ok(Self::new(iterable, IterationMode::Array.marker()));
        }
        Self::get_iterator_slow(cx, iterable, caller)
    }

    #[inline(never)]
    fn get_iterator_slow(cx: &mut Cx, iterable: Value, caller: Caller) -> JsResult<Self> {
        let method = iterable.get(cx, symbols::iterator)?;
        Self::get_iterator_from_method(cx, iterable, method, caller)
    }

    fn get_iterator_from_method(
        cx: &mut Cx,
        iterable: Value,
        method: Value,
        caller: Caller,
    ) -> JsResult<IteratorRecord> {
        if let Some(record) = Self::of_builtin_kind(cx, iterable, method) {
            return Ok(record);
        }
        if caller == Caller::Syntax && !method.is_callable() {
            return Err(throw_not_iterable(cx, iterable));
        }
        let iterator = caller.call(cx, method, iterable, &[])?;
        if !iterator.is_object() {
            return Err(cx.throw_type_error(match caller {
                Caller::Library => errors::TYPE_ERROR,
                Caller::Syntax => errors::ITERATOR_RESULT_NOT_OBJECT,
            }));
        }
        Self::direct(cx, iterator)
    }

    #[inline(always)]
    fn of_builtin_kind(cx: &Cx, iterable: Value, method: Value) -> Option<IteratorRecord> {
        let cell = iterable.as_cell()?;
        if cell.is::<JsArray>() {
            let global = cx.global();
            return (method == global.array_prototype_values().into()
                && global.array_iterator_protocol_is_pristine())
            .then(|| Self::new(iterable, IterationMode::Array.marker()));
        }
        Self::of_other_builtin_kind(cx, cell, method)
    }

    #[inline(never)]
    fn of_other_builtin_kind(cx: &Cx, cell: Gc<JsCell>, method: Value) -> Option<IteratorRecord> {
        let mode = match cell.cell_type() {
            CellType::String => (is_slot(cx, RealmSlot::string_prototype_iterator, method)
                && cx.holds(RealmWatchpoint::StringIteratorProtocol))
            .then_some(IterationMode::String),
            CellType::ArrayIterator => {
                let iterator = cell.cast::<JsArrayIterator>();
                // Compiled code does better with the method for anything else.
                (iterator.iterated_object().is::<JsArray>()
                    && is_slot(cx, RealmSlot::iterator_prototype_iterator, method))
                .then(|| mode_of_iterator(cx, cell))?
            }
            CellType::StringIterator | CellType::MapIterator | CellType::SetIterator => {
                is_slot(cx, RealmSlot::iterator_prototype_iterator, method)
                    .then(|| mode_of_iterator(cx, cell))?
            }
            CellType::Map => {
                return (is_slot(cx, RealmSlot::map_prototype_entries, method)
                    && cx.holds(RealmWatchpoint::MapIteratorProtocol))
                .then(|| cell.cast::<JsMap>().cursor().parts().0)
                .map(|place| Self::new(place.into(), IterationMode::Map.marker()));
            }
            CellType::Set => {
                return (is_slot(cx, RealmSlot::set_prototype_values, method)
                    && cx.holds(RealmWatchpoint::SetIteratorProtocol))
                .then(|| cell.cast::<JsSet>().cursor().parts().0)
                .map(|place| Self::new(place.into(), IterationMode::Set.marker()));
            }
            _ => None,
        }?;
        Some(Self::new(cell.into(), mode.marker()))
    }

    #[inline(always)]
    pub fn mode(&self) -> IterationMode {
        IterationMode::of(self.next)
    }

    #[inline(always)]
    pub fn subject(&self) -> Value {
        self.subject
    }

    #[inline(always)]
    pub fn is_done(&self) -> bool {
        self.position == Self::DONE
    }

    #[inline(always)]
    fn index(&self) -> u32 {
        self.position.to_bits() as u32
    }

    #[inline(always)]
    fn set_index(&mut self, index: u32) {
        self.position = Value::int32(index as i32);
    }

    /// # Safety
    /// The mode says that it is a `T`.
    #[inline(always)]
    unsafe fn subject_as<T>(&self) -> Gc<T> {
        // SAFETY: the caller's.
        unsafe { Gc::from_address(self.subject.to_bits() as usize) }
    }

    /// # Safety
    /// The mode is that of the kind.
    #[inline(always)]
    unsafe fn cursor<K: TableKind>(&self) -> OrderedCursor<K> {
        // SAFETY: the caller's.
        unsafe { OrderedCursor::from_parts_unchecked(self.subject_as(), self.index()) }
    }

    /// `[[Iterator]]`. If there is none, each call makes one that is where the record is.
    pub fn iterator(&self, cx: &Cx) -> Value {
        match self.mode() {
            IterationMode::Array | IterationMode::TypedArray => {
                let (object, kind) = (self.subject.cast(), IterationKind::Values);
                let iterator = JsArrayIterator::create(cx, object, kind);
                iterator.set_index((!self.is_done()).then(|| self.index().into()));
                iterator.into()
            }
            IterationMode::String => {
                let iterator = JsStringIterator::create(cx, self.subject.cast());
                iterator.set_index(self.index());
                iterator.into()
            }
            IterationMode::Map => {
                // SAFETY: of that mode.
                let cursor = unsafe { self.cursor() };
                let iterator = JsMapIterator::create_at(cx, cursor, IterationKind::Entries);
                if self.is_done() {
                    iterator.close(cx);
                }
                iterator.into()
            }
            IterationMode::Set => {
                // SAFETY: as above.
                let cursor = unsafe { self.cursor() };
                let iterator = JsSetIterator::create_at(cx, cursor, IterationKind::Values);
                if self.is_done() {
                    iterator.close(cx);
                }
                iterator.into()
            }
            _ => self.subject,
        }
    }

    /// The next element of an array, if it is there and is plain data.
    #[inline(always)]
    pub fn try_step_array(&mut self) -> Option<Value> {
        if self.next != IterationMode::Array.marker() {
            return None;
        }
        // SAFETY: of that mode.
        let value = unsafe { self.subject_as::<JsArray>() }.try_get_dense(self.index())?;
        self.set_index(self.index() + 1);
        Some(value)
    }

    /// Writes nothing if it throws.
    #[inline(always)]
    fn advance(&mut self, cx: &mut Cx, caller: Caller) -> JsResult<Option<Value>> {
        match self.try_step_array() {
            Some(value) => Ok(Some(value)),
            None => self.advance_slow(cx, caller),
        }
    }

    #[inline(never)]
    fn advance_slow(&mut self, cx: &mut Cx, caller: Caller) -> JsResult<Option<Value>> {
        if IterationMode::is_marker(self.next) {
            return self.step_builtin(cx);
        }
        match self.next_result(cx, caller)? {
            Some(result) => iterator_value(cx, &result).map(Some),
            None => Ok(None),
        }
    }

    #[inline(always)]
    fn next_result(&mut self, cx: &mut Cx, caller: Caller) -> JsResult<Option<Gc<JsObject>>> {
        if self.is_done() {
            return Ok(None);
        }
        let result = caller.next(cx, self.subject, self.next, &[])?;
        if iterator_complete(cx, &result)? {
            self.position = Self::DONE;
            return Ok(None);
        }
        Ok(Some(result))
    }

    /// False if there was nothing left. Writes nothing if it throws.
    fn pass(&mut self, cx: &mut Cx, caller: Caller) -> JsResult<bool> {
        if IterationMode::is_marker(self.next) {
            return Ok(self.step_builtin(cx)?.is_some());
        }
        Ok(self.next_result(cx, caller)?.is_some())
    }

    #[inline(always)]
    fn done_if_abrupt<T>(&mut self, result: JsResult<T>) -> JsResult<T> {
        if result.is_err() {
            self.position = Self::DONE;
        }
        result
    }

    /// IteratorStepValue. `None` stands for done, also from then on.
    #[inline(always)]
    pub fn step_value(&mut self, cx: &mut Cx) -> JsResult<Option<Value>> {
        let result = self.advance(cx, Caller::Library);
        self.done_if_abrupt(result)
    }

    /// IteratorStep, for a caller that does not want the value. False stands for done.
    pub fn step(&mut self, cx: &mut Cx) -> JsResult<bool> {
        let result = self.pass(cx, Caller::Library);
        self.done_if_abrupt(result)
    }

    /// Whether IteratorClose is known to do nothing.
    #[inline(always)]
    pub fn has_nothing_to_close(&self, cx: &Cx) -> bool {
        use IterationMode::{Array, Generic, Map, Set, String, TypedArray};
        let holds = |which| cx.global().watchpoints_unchecked(which).is_still_valid();
        self.is_done()
            || match self.mode() {
                Array | TypedArray => holds(RealmWatchpoint::ArrayIteratorProtocol),
                String => holds(RealmWatchpoint::StringIteratorProtocol),
                Map => holds(RealmWatchpoint::MapIteratorProtocol),
                Set => holds(RealmWatchpoint::SetIteratorProtocol),
                Generic => false,
                _ => mode_of_iterator(cx, self.subject.cast()).is_some(),
            }
    }

    /// IteratorClose with a normal completion, unless it is done.
    #[inline]
    pub fn close(&self, cx: &mut Cx) -> JsResult<()> {
        if self.has_nothing_to_close(cx) {
            return Ok(());
        }
        let iterator = self.iterator(cx);
        Caller::Library.close(cx, iterator)
    }

    /// IteratorClose with a throw completion, unless it is done.
    #[cold]
    pub fn close_on_throw(&self, cx: &mut Cx, thrown: Thrown) -> Thrown {
        if self.has_nothing_to_close(cx) {
            return thrown;
        }
        let iterator = self.iterator(cx);
        iterator_close_on_throw(cx, iterator, thrown)
    }

    /// IfAbruptCloseIterator
    #[inline(always)]
    pub fn close_if_abrupt<T>(&self, cx: &mut Cx, result: JsResult<T>) -> JsResult<T> {
        result.map_err(|thrown| self.close_on_throw(cx, thrown))
    }

    /// Closes the iterator if `visit` throws.
    #[inline(always)]
    pub fn for_each(
        mut self,
        cx: &mut Cx,
        mut visit: impl FnMut(&mut Cx, Value) -> JsResult<()>,
    ) -> JsResult<()> {
        while let Some(value) = self.advance(cx, Caller::Library)? {
            let result = visit(cx, value);
            self.close_if_abrupt(cx, result)?;
        }
        Ok(())
    }

    /// An array of what is left.
    pub fn to_array(mut self, cx: &mut Cx) -> JsResult<Gc<JsArray>> {
        self.collect(cx, Caller::Library)
    }

    fn collect(&mut self, cx: &mut Cx, caller: Caller) -> JsResult<Gc<JsArray>> {
        let mut rest = *self;
        let array = match rest.collect_at_once(cx)? {
            Some(array) => array,
            None => {
                let array = cx.new_array_with_capacity(ElementKind::Undecided, 0)?;
                while let Some(value) = rest.advance(cx, caller)? {
                    if let Err(thrown) = array.append(cx, value) {
                        return Err(rest.close_on_throw(cx, thrown));
                    }
                }
                array
            }
        };
        self.position = Self::DONE;
        Ok(array)
    }

    fn collect_at_once(&self, cx: &mut Cx) -> JsResult<Option<Gc<JsArray>>> {
        use IterationMode::{Map, MapEntries, MapKeys, MapValues, Set, SetEntries, SetValues};
        // SAFETY: each is what the mode says.
        let array = unsafe {
            match self.mode() {
                IterationMode::Array => {
                    let source = self.subject_as::<JsArray>();
                    let length = source.length();
                    let range = self.index().min(length).into()..length.into();
                    return JsArray::try_copy(cx, &source, range, HoleMode::FillWithUndefined);
                }
                _ if self.is_done() => return Ok(None),
                Map => JsMapIterator::rest_to_array(cx, self.cursor(), IterationKind::Entries),
                Set => JsSetIterator::rest_to_array(cx, self.cursor(), IterationKind::Values),
                MapKeys | MapValues | MapEntries => {
                    self.subject_as::<JsMapIterator>().drain_to_array(cx)
                }
                SetValues | SetEntries => self.subject_as::<JsSetIterator>().drain_to_array(cx),
                _ => return Ok(None),
            }
        };
        array.map(Some)
    }
}

/// What the instructions do.
impl IteratorRecord {
    /// `iterator_open`. `None` for the generic mode, in which the position is `START`.
    #[inline]
    pub fn open(cx: &Cx, iterable: Value, method: Value) -> JsResult<Option<IteratorRecord>> {
        let record = Self::of_builtin_kind(cx, iterable, method);
        if record.is_none() && !method.is_callable() {
            return Err(throw_not_iterable(cx, iterable));
        }
        Ok(record)
    }

    /// `iterator_next`. Panics in the generic mode.
    pub fn step_builtin(&mut self, cx: &mut Cx) -> JsResult<Option<Value>> {
        let index = self.index();
        // SAFETY: each is what the mode says.
        let step = unsafe {
            match self.mode() {
                IterationMode::Array => {
                    let array = self.subject_as::<JsArray>();
                    if index < array.length() {
                        Some((array.get_index(cx, index.into())?, index + 1))
                    } else {
                        None
                    }
                }
                IterationMode::String => {
                    code_point_string_at(cx, &self.subject_as::<JsString>(), index)?
                        .map(|(string, next)| (string.into(), next))
                }
                _ if self.is_done() => None,
                IterationMode::TypedArray => {
                    let view = self.subject_as::<JsArrayBufferView>();
                    if (index as usize) < typed_array_length(cx, &view)? {
                        Some((view.get_index(cx, index.into())?, index + 1))
                    } else {
                        None
                    }
                }
                IterationMode::ArrayKeys
                | IterationMode::ArrayValues
                | IterationMode::ArrayEntries => {
                    let value = self.subject_as::<JsArrayIterator>().next(cx)?;
                    value.map(|value| (value, 0))
                }
                IterationMode::Map => {
                    let mut cursor = self.cursor::<MapKind>();
                    match cursor.next() {
                        Some((key, value)) => {
                            let (pair, (place, next)) = (cx.new_pair(key, value)?, cursor.parts());
                            self.subject = place.into();
                            Some((pair.into(), next))
                        }
                        None => None,
                    }
                }
                IterationMode::Set => {
                    let mut cursor = self.cursor::<SetKind>();
                    cursor.next().map(|key| {
                        let (place, next) = cursor.parts();
                        self.subject = place.into();
                        (key, next)
                    })
                }
                IterationMode::MapKeys | IterationMode::MapValues | IterationMode::MapEntries => {
                    let value = self.subject_as::<JsMapIterator>().next(cx)?;
                    value.map(|value| (value, 0))
                }
                IterationMode::SetValues | IterationMode::SetEntries => {
                    let value = self.subject_as::<JsSetIterator>().next(cx)?;
                    value.map(|value| (value, 0))
                }
                IterationMode::StringIterator => {
                    let string = self.subject_as::<JsStringIterator>().next(cx)?;
                    string.map(|string| (string.into(), 0))
                }
                _ => unreachable!(),
            }
        };
        self.set_index(step.map_or(u32::MAX, |(_, next)| next));
        Ok(step.map(|(value, _)| value))
    }

    /// `iterator_begin`
    #[inline]
    pub fn begin(cx: &mut Cx, iterable: Value) -> JsResult<IteratorRecord> {
        Self::get_iterator(cx, iterable, Caller::Syntax)
    }

    /// `iterator_step`
    #[inline]
    pub fn step_in_pattern(&mut self, cx: &mut Cx) -> JsResult<Value> {
        Ok(self.advance(cx, Caller::Syntax)?.unwrap_or_default())
    }

    /// `iterator_skip`
    pub fn skip_in_pattern(&mut self, cx: &mut Cx) -> JsResult<()> {
        self.pass(cx, Caller::Syntax).map(|_| ())
    }

    /// `iterator_rest`
    pub fn rest_in_pattern(&mut self, cx: &mut Cx) -> JsResult<Gc<JsArray>> {
        self.collect(cx, Caller::Syntax)
    }

    /// `iterator_close`
    #[inline]
    pub fn leave(&self, cx: &mut Cx, mode: CloseMode) -> JsResult<()> {
        if self.has_nothing_to_close(cx) {
            return Ok(());
        }
        let iterator = self.iterator(cx);
        match mode {
            CloseMode::Normal => Caller::Syntax.close(cx, iterator),
            CloseMode::Throw => iterator_close_quietly(cx, iterator),
        }
    }
}

/// `None` unless its `next` is the original one.
fn mode_of_iterator(cx: &Cx, iterator: Gc<JsCell>) -> Option<IterationMode> {
    let has_shape = |slot| is_slot(cx, slot, iterator.shape().into());
    match iterator.cell_type() {
        CellType::ArrayIterator => (has_shape(RealmSlot::array_iterator_shape)
            && cx.global().array_iterator_protocol_is_pristine())
        .then(|| match iterator.cast::<JsArrayIterator>().kind() {
            IterationKind::Keys => IterationMode::ArrayKeys,
            IterationKind::Values => IterationMode::ArrayValues,
            IterationKind::Entries => IterationMode::ArrayEntries,
        }),
        CellType::StringIterator => (has_shape(RealmSlot::string_iterator_shape)
            && cx.holds(RealmWatchpoint::StringIteratorProtocol))
        .then_some(IterationMode::StringIterator),
        CellType::MapIterator => (has_shape(RealmSlot::map_iterator_shape)
            && cx.holds(RealmWatchpoint::MapIteratorProtocol))
        .then(|| match iterator.cast::<JsMapIterator>().kind() {
            IterationKind::Keys => IterationMode::MapKeys,
            IterationKind::Values => IterationMode::MapValues,
            IterationKind::Entries => IterationMode::MapEntries,
        }),
        CellType::SetIterator => (has_shape(RealmSlot::set_iterator_shape)
            && cx.holds(RealmWatchpoint::SetIteratorProtocol))
        .then(|| match iterator.cast::<JsSetIterator>().kind() {
            IterationKind::Keys | IterationKind::Values => IterationMode::SetValues,
            IterationKind::Entries => IterationMode::SetEntries,
        }),
        _ => None,
    }
}

/// Closes the iterator if `visit` throws.
#[inline(always)]
pub fn for_each_in_iterable(
    cx: &mut Cx,
    iterable: Value,
    visit: impl FnMut(&mut Cx, Value) -> JsResult<()>,
) -> JsResult<()> {
    IteratorRecord::for_iterable(cx, iterable)?.for_each(cx, visit)
}
