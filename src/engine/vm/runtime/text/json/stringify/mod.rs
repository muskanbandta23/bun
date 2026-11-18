//! `JSON.stringify`.

mod options;
mod steps;

use bun_engine_text::{Identifier, PropertyName, StringBuilder, StringRef};

use crate::{Attributes, Cx, FastHashSet, Gc, JsObject, JsResult, RootVec, Value, root_vec};
use options::{Gap, Replacer};

const MAX_DEPTH: usize = 40_000;

/// `None` stands for `undefined`.
pub fn stringify(
    cx: &mut Cx,
    value: Value,
    replacer: Value,
    space: Value,
) -> JsResult<Option<StringRef>> {
    const BYTES_ON_STACK: usize = 2048;
    let mut writer = Writer {
        replacer: Replacer::new(cx, replacer)?,
        gap: Gap::new(cx, space)?,
        holders: root_vec(),
        ancestors: FastHashSet::default(),
    };
    StringBuilder::on_stack::<BYTES_ON_STACK, _>(|out| {
        if !writer.write_root(cx, out, value)? {
            return Ok(None);
        }
        cx.or_out_of_memory(out.take()).map(Some)
    })
}

/// An object or an array whose members are being written.
struct Holder {
    object: Gc<JsObject>,
    members: Members,
    next: u32,
    count: u32,
    has_written: bool,
}

enum Members {
    Elements,
    Named(Vec<Identifier>),
    /// Those of the replacer.
    Listed,
}

struct Writer {
    replacer: Replacer,
    gap: Gap,
    holders: RootVec<Holder>,
    /// The addresses of the holders from `Writer::SEARCHED` on.
    ancestors: FastHashSet<usize>,
}

impl Writer {
    /// False if there is no text for the value.
    fn write_root(&mut self, cx: &mut Cx, out: &mut StringBuilder, value: Value) -> JsResult<bool> {
        let mut wrapper = Value::UNDEFINED;
        if let Replacer::Function(_) = self.replacer {
            let object = cx.new_object();
            object.put_direct(cx, PropertyName::EMPTY, value, Attributes::NONE);
            wrapper = object.into();
        }
        let key = steps::Key::Name(PropertyName::EMPTY);
        let Some(value) = self.resolve(cx, wrapper, key, value)? else {
            return Ok(false);
        };
        self.write_resolved(cx, out, value)?;
        while !self.holders.is_empty() && !out.has_overflowed() {
            cx.check_traps()?;
            self.step(cx, out)?;
        }
        Ok(true)
    }
}
