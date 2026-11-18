use bun_engine_regexp::Flags;
use bun_engine_text::PropertyName;
use bun_engine_vm::RegExpObject;

use super::this_object;
use crate::prelude::*;

/// In the order of the bits of `Flags`.
pub(super) const FLAG_NAMES: [PropertyName<'static>; 8] = [
    names::hasIndices,
    names::global,
    names::ignoreCase,
    names::multiline,
    names::dotAll,
    names::unicode,
    names::unicodeSets,
    names::sticky,
];

/// `None` for the prototype.
#[inline(always)]
fn this_regexp(
    cx: &Cx,
    frame: &CallFrame,
    message: &'static str,
) -> JsResult<Option<Gc<RegExpObject>>> {
    let this = frame.this_value();
    match this.downcast() {
        None if this != cx.regexp_prototype().into() => Err(cx.throw_type_error(message)),
        regexp => Ok(regexp),
    }
}

macro_rules! message {
    ($name:literal) => {
        concat!(
            "The RegExp.prototype.",
            $name,
            " getter can only be called on a RegExp object"
        )
    };
}

macro_rules! flag_getters {
    ($($function:ident, $name:literal, $flag:ident;)*) => {$(
        #[native_abi]
        pub(super) fn $function(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
            let this = this_regexp(cx, frame, message!($name))?;
            Ok(this.map_or_default(|it| it.regexp().flags().contains(Flags::$flag).into()))
        }
    )*};
}

flag_getters! {
    has_indices, "hasIndices", HAS_INDICES;
    global, "global", GLOBAL;
    ignore_case, "ignoreCase", IGNORE_CASE;
    multiline, "multiline", MULTILINE;
    dot_all, "dotAll", DOT_ALL;
    unicode, "unicode", UNICODE;
    unicode_sets, "unicodeSets", UNICODE_SETS;
    sticky, "sticky", STICKY;
}

#[native_abi]
pub(super) fn source(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    Ok(match this_regexp(cx, frame, message!("source"))? {
        Some(this) => this.regexp().source(cx)?.into(),
        None => JsString::from_literal(cx, "(?:)").into(),
    })
}

#[native_abi]
pub(super) fn flags(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let message = "The RegExp.prototype.flags getter can only be called on an object";
    let this = this_object(cx, frame, message)?;
    let mut flags = Flags::empty();
    match this.downcast::<RegExpObject>() {
        Some(this) if this.inherits_pristine(cx.in_realm_of(&this), &FLAG_NAMES) => {
            flags = this.regexp().flags();
        }
        _ => {
            for (name, flag) in FLAG_NAMES.into_iter().zip(Flags::all()) {
                flags.set(flag, this.get(cx, name)?.to_boolean(cx));
            }
        }
    }
    Ok(JsString::from_text(cx, flags.to_text().text()).into())
}
