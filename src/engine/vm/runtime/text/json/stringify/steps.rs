//! One member at a time, as ECMA-262 has it.

use bun_engine_number::NumberText;
use bun_engine_text::{PropertyName, StringBuilder, names};

use super::options::Replacer;
use super::{Holder, MAX_DEPTH, Members, Writer};
use crate::json::RawJson;
use crate::{
    BigIntObject, BooleanObject, Cx, DontEnumMode, Gc, JsObject, JsResult, JsString,
    JsWrapperObject, NumberObject, PropertyNameMode, SpecialProperty, StringObject, Value,
    is_array,
};

#[derive(Clone, Copy)]
pub(super) enum Key<'a> {
    Index(u32),
    Name(PropertyName<'a>),
}

impl Key<'_> {
    fn to_value(self, cx: &Cx) -> Value {
        match self {
            Key::Index(index) => JsString::from_number(cx, index.into()).into(),
            Key::Name(name) => JsString::from_name(cx, name).into(),
        }
    }
}

fn unwrap(cx: &mut Cx, object: Gc<JsObject>) -> JsResult<Value> {
    let value = Value::from(object);
    Ok(if object.is::<NumberObject>() {
        value.to_number(cx)?.into()
    } else if object.is::<StringObject>() {
        value.to_string(cx)?.into()
    } else if object.is::<BooleanObject>() || object.is::<BigIntObject>() {
        object.cast::<JsWrapperObject>().internal_value()
    } else {
        value
    })
}

impl Writer {
    /// Up to this depth a cycle is found by going through the holders.
    const SEARCHED: usize = 32;

    /// SerializeJSONProperty up to where it is known what to write, if anything.
    pub(super) fn resolve(
        &self,
        cx: &mut Cx,
        holder: Value,
        key: Key<'_>,
        mut value: Value,
    ) -> JsResult<Option<Value>> {
        let to_json = if let Some(object) = value.as_object() {
            object.get_special_property(cx, SpecialProperty::ToJson)?
        } else if value.is_bigint() {
            value.get(cx, names::toJSON)?
        } else {
            Value::UNDEFINED
        };
        if to_json.is_callable() {
            value = cx.call(to_json, value, &[key.to_value(cx)])?;
        }
        if let Replacer::Function(replacer) = self.replacer {
            value = cx.call(replacer, holder, &[key.to_value(cx), value])?;
        }
        if let Some(object) = value.as_object() {
            value = unwrap(cx, object)?;
        }
        if value.is_bigint() {
            return Err(cx.throw_type_error("JSON.stringify cannot serialize BigInt."));
        }
        Ok((!value.is_undefined() && !value.is_symbol() && !value.is_callable()).then_some(value))
    }

    /// Of an object or an array, only the beginning.
    pub(super) fn write_resolved(
        &mut self,
        cx: &mut Cx,
        out: &mut StringBuilder,
        value: Value,
    ) -> JsResult<()> {
        if let Some(string) = value.as_string() {
            out.push_json_quoted(string.view(cx)?.text());
        } else if let Some(raw) = value.downcast::<RawJson>() {
            out.push_text(raw.text().view(cx)?.text());
        } else if let Some(object) = value.as_object() {
            self.open(cx, out, object)?;
        } else if let Some(integer) = value.as_int32() {
            out.push_latin1_slice(&NumberText::from(integer));
        } else if let Some(number) = value.as_double().filter(|it| it.is_finite()) {
            out.push_latin1_slice(&NumberText::from(number));
        } else {
            out.push_ascii(match value {
                Value::TRUE => "true",
                Value::FALSE => "false",
                _ => "null",
            });
        }
        Ok(())
    }

    fn open(&mut self, cx: &mut Cx, out: &mut StringBuilder, object: Gc<JsObject>) -> JsResult<()> {
        let depth = self.holders.len();
        let near = &self.holders[..depth.min(Self::SEARCHED)];
        if near.iter().any(|holder| holder.object == object)
            || (depth >= Self::SEARCHED && !self.ancestors.insert(object.address()))
        {
            let message = "JSON.stringify cannot serialize cyclic structures.";
            return Err(cx.throw_type_error(message));
        }
        if depth >= MAX_DEPTH {
            return Err(cx.throw_stack_overflow());
        }
        let (members, count) = if is_array(cx, object.into())? {
            (Members::Elements, object.length_of_array_like(cx)?)
        } else if let Replacer::List(names) = &self.replacer {
            (Members::Listed, names.len() as u64)
        } else {
            let names = object.own_keys(cx, PropertyNameMode::Strings, DontEnumMode::Exclude)?;
            let count = names.len() as u64;
            (Members::Named(names.into_vec()), count)
        };
        let count = u32::try_from(count).map_err(|_| cx.throw_out_of_memory())?;
        out.push_latin1(match members {
            Members::Elements => b'[',
            _ => b'{',
        });
        self.holders.push(Holder {
            object,
            members,
            next: 0,
            count,
            has_written: false,
        });
        Ok(())
    }

    fn close(&mut self, out: &mut StringBuilder) {
        let Some(holder) = self.holders.pop() else {
            return;
        };
        if self.holders.len() >= Self::SEARCHED {
            self.ancestors.remove(&holder.object.address());
        }
        if holder.has_written {
            self.start_line(out);
        }
        out.push_latin1(match holder.members {
            Members::Elements => b']',
            _ => b'}',
        });
    }

    fn start_line(&self, out: &mut StringBuilder) {
        if !self.gap.is_empty() {
            out.push_latin1(b'\n');
            for _ in 0..self.holders.len() {
                out.push_text(self.gap.text());
            }
        }
    }

    /// Writes the next member of the innermost holder, or its end.
    pub(super) fn step(&mut self, cx: &mut Cx, out: &mut StringBuilder) -> JsResult<()> {
        let Some(holder) = self.holders.last_mut().filter(|it| it.next < it.count) else {
            self.close(out);
            return Ok(());
        };
        let (object, index) = (holder.object, holder.next);
        holder.next += 1;
        let name = match (&holder.members, &self.replacer) {
            (Members::Named(names), _) | (Members::Listed, Replacer::List(names)) => {
                Some(names[index as usize].clone())
            }
            _ => None,
        };
        let value = match &name {
            Some(name) => {
                let value = object.get(cx, name.name())?;
                match self.resolve(cx, object.into(), Key::Name(name.name()), value)? {
                    Some(value) => value,
                    None => return Ok(()),
                }
            }
            None => {
                let value = object.get_index(cx, index.into())?;
                self.resolve(cx, object.into(), Key::Index(index), value)?
                    .unwrap_or(Value::NULL)
            }
        };
        if let Some(holder) = self.holders.last_mut()
            && core::mem::replace(&mut holder.has_written, true)
        {
            out.push_latin1(b',');
        }
        self.start_line(out);
        if let Some(name) = name {
            out.push_json_quoted(name.name().text());
            out.push_ascii(if self.gap.is_empty() { ":" } else { ": " });
        }
        self.write_resolved(cx, out, value)
    }
}
