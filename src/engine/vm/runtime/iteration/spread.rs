use bun_engine_text::{Text, symbols};

use crate::{
    Cx, ElementKind, ElementWriter, Gc, HoleMode, ImmutableArray, IterationMode, IteratorRecord,
    JsArray, JsObject, JsResult, JsSet, JsString, RealmWatchpoint, Value, ValueVec, errors,
};

/// The elements up to `length`, unless getting them takes more than a look at the storage. Never raw doubles.
pub(super) fn try_list_of_elements(
    cx: &mut Cx,
    source: &JsObject,
    length: u64,
) -> JsResult<Option<Gc<ImmutableArray>>> {
    if let Some(shared) = source.shared_elements()
        && shared.kind() != ElementKind::Double
        && u64::from(shared.len()) == length
    {
        return Ok(Some(shared));
    }
    let (no_gc, holes) = (cx.no_gc(), HoleMode::FillWithUndefined);
    let Some(elements) = source.plain_elements(&no_gc, 0..length) else {
        return Ok(None);
    };
    let kind = match elements.kind_with(holes) {
        ElementKind::Int32 => ElementKind::Int32,
        _ => ElementKind::Contiguous,
    };
    // SAFETY: an allocation leaves the elements as they are, and they are only copied. No hole is left.
    let list = unsafe {
        ImmutableArray::try_build(
            cx,
            kind,
            length as usize,
            |writer: &mut ElementWriter<'_>| {
                let elements = source.elements_unchecked().expect("dense elements");
                writer.extend(elements.slice(0..length as usize), holes);
            },
        )
    };
    cx.or_out_of_memory(list).map(Some)
}

fn list_of_code_points(cx: &Cx, string: &JsString) -> JsResult<Gc<ImmutableArray>> {
    let view = string.view(cx)?;
    cx.or_out_of_memory(match view.text() {
        Text::Latin1(characters) => ImmutableArray::try_from_fn(cx, characters.len(), |index| {
            (cx.small_strings.single_character(characters[index])).into()
        }),
        text => {
            let mut code_points = text.code_points();
            ImmutableArray::try_from_fn(cx, code_points.clone().count(), |_| {
                JsString::from_code_point(cx, code_points.next().unwrap_or_default()).into()
            })
        }
    })
}

/// What `iterable` yields, if no script can tell how that was found out.
pub fn try_spread_directly(cx: &mut Cx, iterable: Value) -> JsResult<Option<Gc<ImmutableArray>>> {
    if let Some(array) = iterable.downcast::<JsArray>() {
        if array.is_iteration_pristine() {
            return try_list_of_elements(cx, &array, array.length().into());
        }
    } else if let Some(string) = iterable.as_string()
        && cx.holds(RealmWatchpoint::StringIteratorProtocol)
    {
        return list_of_code_points(cx, &string).map(Some);
    } else if let Some(set) = iterable.downcast::<JsSet>()
        && set.is_iteration_pristine(cx)
    {
        return set.to_immutable_array(cx).map(Some);
    }
    Ok(None)
}

/// `spread`. The elements of the list are values, not raw doubles.
#[inline]
pub fn spread(cx: &mut Cx, iterable: Value) -> JsResult<Gc<ImmutableArray>> {
    match try_spread_directly(cx, iterable)? {
        Some(list) => Ok(list),
        None => spread_slow(cx, iterable),
    }
}

#[inline(never)]
fn spread_slow(cx: &mut Cx, iterable: Value) -> JsResult<Gc<ImmutableArray>> {
    if iterable.is_undefined_or_null() {
        return Err(
            cx.throw_type_error("Spread syntax requires ...iterable not be null or undefined")
        );
    }
    let method = iterable.get(cx, symbols::iterator)?;
    if !method.is_callable() {
        let message = "Spread syntax requires ...iterable[Symbol.iterator] to be a function";
        return Err(cx.throw_type_error(message));
    }
    let mut record = match IteratorRecord::open(cx, iterable, method)? {
        Some(record) => record,
        None => {
            let iterator = cx.call(method, iterable, &[])?;
            IteratorRecord::direct(cx, iterator)?
        }
    };
    if record.mode() == IterationMode::Generic {
        let [_, next, iterator] = record.parts();
        if !iterator.is_object() && next.is_callable() {
            return Err(cx.throw_type_error(errors::ITERATOR_RESULT_NOT_OBJECT));
        }
        let collect = cx.private_collect_for_spread().into();
        let array = cx.call(collect, Value::UNDEFINED, &[iterator, next])?;
        let array = array.cast::<JsArray>();
        let list = try_list_of_elements(cx, &array, array.length().into())?;
        return list.ok_or_else(|| cx.throw_out_of_memory());
    }
    let mut values = ValueVec::new();
    while let Some(value) = record.step_builtin(cx)? {
        cx.or_out_of_memory(values.try_push(value))?;
    }
    cx.or_out_of_memory(ImmutableArray::try_from_values(cx, &values))
}
