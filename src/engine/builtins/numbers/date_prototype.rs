use bun_engine_date::{
    Component, DateFields, date_string, iso_string, make_full_year, ms_from_time, utc_string,
};
use bun_engine_intl::date_time_format::Purpose;
use bun_engine_text::StringRef;
use bun_engine_vm::{Attributes, DateInstance, JsFunction, PreferredType};

use crate::prelude::*;

macro_rules! getter {
    ($name:ident, $function:ident, $intrinsic:ident) => {
        StaticProperty::function(names::$name, $function, 0).with_intrinsic(Intrinsic::$intrinsic)
    };
}

static_table!(static PROPERTIES = [
    StaticProperty::function(names::toString, to_string, 0),
    StaticProperty::function(names::toISOString, to_iso_string, 0),
    StaticProperty::function(names::toDateString, to_date_string, 0),
    StaticProperty::function(names::toTimeString, to_time_string, 0),
    StaticProperty::function(names::toLocaleString, to_locale_string, 0),
    StaticProperty::function(names::toLocaleDateString, to_locale_date_string, 0),
    StaticProperty::function(names::toLocaleTimeString, to_locale_time_string, 0),
    getter!(valueOf, get_time, DatePrototypeGetTime),
    getter!(getTime, get_time, DatePrototypeGetTime),
    getter!(getFullYear, get_full_year, DatePrototypeGetFullYear),
    getter!(getUTCFullYear, get_utc_full_year, DatePrototypeGetUTCFullYear),
    getter!(getMonth, get_month, DatePrototypeGetMonth),
    getter!(getUTCMonth, get_utc_month, DatePrototypeGetUTCMonth),
    getter!(getDate, get_date, DatePrototypeGetDate),
    getter!(getUTCDate, get_utc_date, DatePrototypeGetUTCDate),
    getter!(getDay, get_day, DatePrototypeGetDay),
    getter!(getUTCDay, get_utc_day, DatePrototypeGetUTCDay),
    getter!(getHours, get_hours, DatePrototypeGetHours),
    getter!(getUTCHours, get_utc_hours, DatePrototypeGetUTCHours),
    getter!(getMinutes, get_minutes, DatePrototypeGetMinutes),
    getter!(getUTCMinutes, get_utc_minutes, DatePrototypeGetUTCMinutes),
    getter!(getSeconds, get_seconds, DatePrototypeGetSeconds),
    getter!(getUTCSeconds, get_utc_seconds, DatePrototypeGetUTCSeconds),
    getter!(getMilliseconds, get_milliseconds, DatePrototypeGetMilliseconds),
    getter!(getUTCMilliseconds, get_milliseconds, DatePrototypeGetUTCMilliseconds),
    getter!(getTimezoneOffset, get_timezone_offset, DatePrototypeGetTimezoneOffset),
    getter!(getYear, get_year, DatePrototypeGetYear),
    StaticProperty::function(names::setTime, set_time, 1)
        .with_intrinsic(Intrinsic::DatePrototypeSetTime),
    StaticProperty::function(names::setMilliseconds, set_milliseconds, 1),
    StaticProperty::function(names::setUTCMilliseconds, set_utc_milliseconds, 1),
    StaticProperty::function(names::setSeconds, set_seconds, 2),
    StaticProperty::function(names::setUTCSeconds, set_utc_seconds, 2),
    StaticProperty::function(names::setMinutes, set_minutes, 3),
    StaticProperty::function(names::setUTCMinutes, set_utc_minutes, 3),
    StaticProperty::function(names::setHours, set_hours, 4),
    StaticProperty::function(names::setUTCHours, set_utc_hours, 4),
    StaticProperty::function(names::setDate, set_date, 1),
    StaticProperty::function(names::setUTCDate, set_utc_date, 1),
    StaticProperty::function(names::setMonth, set_month, 2),
    StaticProperty::function(names::setUTCMonth, set_utc_month, 2),
    StaticProperty::function(names::setFullYear, set_full_year, 3),
    StaticProperty::function(names::setUTCFullYear, set_utc_full_year, 3),
    StaticProperty::function(names::setYear, set_year, 1),
    StaticProperty::function(names::toJSON, to_json, 1),
    StaticProperty::function(symbols::toPrimitive, to_primitive, 1).with(Attributes::READ_ONLY),
]);

define_object!(DatePrototype, "Object", PROPERTIES);

pub(super) fn create(cx: &Cx) -> Gc<JsObject> {
    let prototype = DatePrototype::create(cx, cx.object_prototype().into());
    let name = names::toUTCString.to_identifier().into_string();
    let function = JsFunction::native(cx, 0, &name, to_utc_string);
    for name in [names::toUTCString, names::toGMTString] {
        prototype.put_direct(cx, name, function.into(), Attributes::DONT_ENUM);
    }
    prototype
}

#[inline(always)]
fn this_date(cx: &Cx, frame: &CallFrame) -> JsResult<Gc<DateInstance>> {
    (frame.this_value().downcast()).ok_or_else(|| cx.throw_type_error("Type error"))
}

pub(super) fn or_invalid_date(cx: &Cx, string: Option<StringRef>) -> Value {
    match string {
        Some(string) => JsString::new(cx, string),
        None => JsString::from_name(cx, names::invalidDate),
    }
    .into()
}

#[native_abi]
fn to_string(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let fields = this_date(cx, frame)?.local_fields(cx);
    let string = fields.map(|fields| cx.date_cache().date_time_string(fields));
    Ok(or_invalid_date(cx, string))
}

#[native_abi]
fn to_date_string(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let fields = this_date(cx, frame)?.local_fields(cx);
    Ok(or_invalid_date(cx, fields.map(date_string)))
}

#[native_abi]
fn to_time_string(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let fields = this_date(cx, frame)?.local_fields(cx);
    let string = fields.map(|fields| cx.date_cache().time_string(fields));
    Ok(or_invalid_date(cx, string))
}

fn format_for_locale(cx: &mut Cx, frame: &CallFrame, purpose: Purpose) -> JsResult<Value> {
    let time = this_date(cx, frame)?.time();
    if time.is_nan() {
        return Ok(or_invalid_date(cx, None));
    }
    let (locales, options) = (frame.argument(0), frame.argument(1));
    bun_engine_intl::date_to_locale_string(cx, time, purpose, locales, options)
}

#[native_abi]
fn to_locale_string(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    format_for_locale(cx, frame, Purpose::DateAndTime)
}

#[native_abi]
fn to_locale_date_string(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    format_for_locale(cx, frame, Purpose::Date)
}

#[native_abi]
fn to_locale_time_string(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    format_for_locale(cx, frame, Purpose::Time)
}

#[native_abi]
fn to_utc_string(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let fields = this_date(cx, frame)?.utc_fields(cx);
    Ok(or_invalid_date(cx, fields.map(utc_string)))
}

#[native_abi]
fn to_iso_string(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    match iso_string(this_date(cx, frame)?.time()) {
        Some(string) => Ok(JsString::new(cx, string).into()),
        None => Err(cx.throw_range_error("Invalid Date")),
    }
}

#[native_abi]
fn to_json(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let object = frame.this_value().to_object(cx)?;
    let time = object.to_primitive(cx, PreferredType::Number)?;
    if time.as_number().is_some_and(|time| !time.is_finite()) {
        return Ok(Value::NULL);
    }
    let method = object.get(cx, names::toISOString)?;
    if !method.is_callable() {
        return Err(cx.throw_type_error("toISOString is not a function"));
    }
    cx.call(method, object.into(), &[])
}

#[native_abi]
fn to_primitive(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let Some(object) = frame.this_value().as_object() else {
        let message = "Date.prototype[Symbol.toPrimitive] expected |this| to be an object.";
        return Err(cx.throw_type_error(message));
    };
    let Some(hint) = frame.arguments().first() else {
        let message = "Date.prototype[Symbol.toPrimitive] expected a first argument.";
        return Err(cx.throw_type_error(message));
    };
    let Some(hint) = hint.as_string() else {
        return Err(cx.throw_type_error("Primitive hint is not a string."));
    };
    let hint = hint.view(cx)?;
    let preferred = if hint.text().eq_ascii("default") || hint.text().eq_ascii("string") {
        PreferredType::String
    } else if hint.text().eq_ascii("number") {
        PreferredType::Number
    } else {
        let message = "Expected primitive hint to match one of 'default', 'number', 'string'.";
        return Err(cx.throw_type_error(message));
    };
    object.ordinary_to_primitive(cx, preferred)
}

#[native_abi]
fn get_time(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    Ok(this_date(cx, frame)?.time().into())
}

#[native_abi]
fn get_milliseconds(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let time = this_date(cx, frame)?.time();
    if time.is_nan() {
        return Ok(Value::NAN);
    }
    Ok(ms_from_time(time).into())
}

macro_rules! getters {
    ($($name:ident = $fields:ident, $read:expr;)*) => {$(
        #[native_abi]
        fn $name(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
            let fields = this_date(cx, frame)?.$fields(cx);
            Ok(fields.map($read).map_or(Value::NAN, Value::from))
        }
    )*};
}

getters! {
    get_full_year = local_fields, DateFields::year;
    get_utc_full_year = utc_fields, DateFields::year;
    get_month = local_fields, DateFields::month;
    get_utc_month = utc_fields, DateFields::month;
    get_date = local_fields, DateFields::day;
    get_utc_date = utc_fields, DateFields::day;
    get_day = local_fields, DateFields::week_day;
    get_utc_day = utc_fields, DateFields::week_day;
    get_hours = local_fields, DateFields::hour;
    get_utc_hours = utc_fields, DateFields::hour;
    get_minutes = local_fields, DateFields::minute;
    get_utc_minutes = utc_fields, DateFields::minute;
    get_seconds = local_fields, DateFields::second;
    get_utc_seconds = utc_fields, DateFields::second;
    get_timezone_offset = local_fields, |fields| -fields.offset_minutes();
    get_year = local_fields, |fields| fields.year() - 1900;
}

#[native_abi]
fn set_time(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let date = this_date(cx, frame)?;
    let time = frame.argument(0).to_number(cx)?;
    Ok(date.set_time(time).into())
}

#[derive(Clone, Copy)]
enum Clock {
    Local,
    Utc,
}

/// What a setter starts from. It is read before the arguments are converted, which can change the date.
struct Start {
    date: Gc<DateInstance>,
    clock: Clock,
    fields: Option<DateFields>,
    millisecond: u16,
}

impl Start {
    #[inline(always)]
    fn new(cx: &Cx, frame: &CallFrame, clock: Clock) -> JsResult<Start> {
        let date = this_date(cx, frame)?;
        Ok(Start {
            date,
            clock,
            fields: match clock {
                Clock::Local => date.local_fields(cx),
                Clock::Utc => date.utc_fields(cx),
            },
            millisecond: ms_from_time(date.time()),
        })
    }

    #[inline(always)]
    fn finish(self, cx: &Cx, first: Component, values: &[f64]) -> Value {
        let fields = match (self.fields, first) {
            (Some(fields), _) => fields,
            (None, Component::Year) => DateFields::EPOCH,
            (None, _) => return Value::NAN,
        };
        let time = fields.with_components(self.millisecond, first, values);
        let time = match self.clock {
            Clock::Local => cx.date_cache().utc(time),
            Clock::Utc => time,
        };
        self.date.set_time(time).into()
    }
}

/// From `first` to the end of the date or of the time of day, as far as there are arguments.
#[inline(never)]
fn set_components(
    cx: &mut Cx,
    frame: &CallFrame,
    clock: Clock,
    first: Component,
) -> JsResult<Value> {
    let start = Start::new(cx, frame, clock)?;
    let last = match first {
        Component::Year | Component::Month | Component::Day => Component::Day,
        _ => Component::Millisecond,
    };
    let count = (frame.argument_count()).clamp(1, last as usize - first as usize + 1);
    let mut values = [f64::NAN; 4];
    for (value, argument) in values[..count].iter_mut().zip(frame.arguments()) {
        *value = argument.to_number(cx)?;
    }
    Ok(start.finish(cx, first, &values[..count]))
}

macro_rules! setters {
    ($($name:ident = $clock:ident, $first:ident;)*) => {$(
        #[native_abi]
        fn $name(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
            set_components(cx, frame, Clock::$clock, Component::$first)
        }
    )*};
}

setters! {
    set_milliseconds = Local, Millisecond;
    set_utc_milliseconds = Utc, Millisecond;
    set_seconds = Local, Second;
    set_utc_seconds = Utc, Second;
    set_minutes = Local, Minute;
    set_utc_minutes = Utc, Minute;
    set_hours = Local, Hour;
    set_utc_hours = Utc, Hour;
    set_date = Local, Day;
    set_utc_date = Utc, Day;
    set_month = Local, Month;
    set_utc_month = Utc, Month;
    set_full_year = Local, Year;
    set_utc_full_year = Utc, Year;
}

#[native_abi]
fn set_year(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let start = Start::new(cx, frame, Clock::Local)?;
    let year = make_full_year(frame.argument(0).to_number(cx)?);
    Ok(start.finish(cx, Component::Year, &[year]))
}
