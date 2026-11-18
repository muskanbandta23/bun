//! Keys that were not foreseen, and objects whose shape is not known yet.

use bun_alloc::AllocError;
use bun_engine_text::{CodeUnit, Identifier, PropertyName, Text, parse_array_index};
use smallvec::SmallVec;

use super::keys::{KeyTree, NodeId};
use super::{Parser, Tables, skip_white_space};
use crate::json::records::Recorder;
use crate::json::syntax_error::Expected;
use crate::json::{Encoding, Failure};
use crate::{Attributes, Cx, Gc, JsFinalObject, JsObject, Shape, Value};

/// Of an object outside the tree.
pub(super) enum Key {
    Name(Identifier),
    Index(u32),
}

impl Key {
    fn name(&self) -> Option<PropertyName<'_>> {
        match self {
            Key::Name(name) => Some(name.name()),
            Key::Index(_) => None,
        }
    }
}

impl Tables {
    /// Where the key `text` leads from `parent`.
    #[inline(always)]
    fn follow(&mut self, parent: NodeId, text: Text<'_>) -> Result<NodeId, AllocError> {
        let index = parse_array_index(text);
        if parent != NodeId::OUTSIDE {
            if index.is_none()
                && let Some(node) = self.tree.follow(parent, text, text.hash())
            {
                return Ok(node);
            }
            let (start, depth) = (self.keys.len(), self.tree[parent].depth);
            self.keys.try_reserve(depth.into()).map_err(|_| AllocError)?;
            let path = self.tree.path(parent);
            (self.keys).extend(path.map(|name| Key::Name(name.to_identifier())));
            self.keys[start..].reverse();
            self.tree[NodeId::OUTSIDE].child = self.tree[parent].child;
        }
        self.keys.try_reserve(1).map_err(|_| AllocError)?;
        self.keys.push(match index {
            Some(index) => Key::Index(index),
            None => Key::Name(Identifier::new(text)),
        });
        Ok(NodeId::OUTSIDE)
    }

    pub(super) fn identifier(&self, node: NodeId) -> Identifier {
        match self.keys.last().filter(|_| node == NodeId::OUTSIDE) {
            Some(Key::Name(name)) => name.clone(),
            Some(&Key::Index(index)) => Identifier::from_index(index),
            None => self.tree[node].name().to_identifier(),
        }
    }

    /// An object that ends at `node`, which has no shape.
    #[inline(never)]
    pub(super) fn create_object(
        &mut self,
        cx: &Cx,
        node: NodeId,
        values: &mut [Value],
    ) -> Result<Gc<JsObject>, AllocError> {
        if node == NodeId::OUTSIDE {
            let first = self.keys.len() - values.len();
            let object = object_with_keys(cx, &self.keys[first..], values);
            self.keys.truncate(first);
            return object;
        }
        let mut names: SmallVec<[_; 32]> = self.tree.path(node).collect();
        names.reverse();
        let Some(shape) = shape_with(cx, &names) else {
            return Ok(object_with_names(cx, names.into_iter(), values));
        };
        drop(names);
        self.tree.set_shape(node, &shape);
        JsFinalObject::try_create_with_values(cx, shape, values).map(Gc::upcast)
    }
}

/// `None` if a name repeats.
fn shape_with(cx: &Cx, names: &[PropertyName<'_>]) -> Option<Gc<Shape>> {
    let empty = Shape::for_new_object(cx, names.len());
    names.iter().try_fold(empty, |shape, &name| {
        (shape.existing_add_transition(name, Attributes::NONE)).or_else(|| {
            let is_new = shape.get(name).is_none();
            is_new.then(|| shape.add_property_transition(cx, name, Attributes::NONE).0)
        })
    })
}

fn object_with_names<'a>(
    cx: &Cx,
    names: impl Iterator<Item = PropertyName<'a>>,
    values: &[Value],
) -> Gc<JsObject> {
    let object = JsFinalObject::create(cx, Shape::for_new_object(cx, values.len()));
    for (name, &value) in names.zip(values) {
        object.put_direct(cx, name, value, Attributes::NONE);
    }
    object.upcast()
}

fn object_with_keys(cx: &Cx, keys: &[Key], values: &mut [Value]) -> Result<Gc<JsObject>, AllocError> {
    let mut elements = Vec::new();
    let mut named = 0;
    for (position, key) in keys.iter().enumerate() {
        if let Key::Index(index) = *key {
            elements.try_reserve(1).map_err(|_| AllocError)?;
            elements.push((index, values[position]));
        } else {
            values[named] = values[position];
            named += 1;
        }
    }
    let (names, values) = (keys.iter().filter_map(Key::name), &mut values[..named]);
    let object = if named > usize::from(KeyTree::MAX_DEPTH) {
        JsFinalObject::try_create_dictionary(cx, names, values)?.upcast()
    } else {
        object_with_names(cx, names, values)
    };
    for (index, value) in elements {
        if !object.try_set_dense(index, value) {
            object.store_element(cx, index, value)?;
        }
    }
    Ok(object)
}

impl<'a, E: Encoding, R: Recorder> Parser<'a, E, R> {
    /// Reads a key and the colon. Returns their length and where the key leads from `parent`.
    #[inline(never)]
    pub(super) fn unforeseen_key(
        &mut self,
        rest: &[E::Unit],
        parent: NodeId,
        expected: Expected,
    ) -> Result<(usize, NodeId), Failure> {
        if !rest.first().is_some_and(|unit| unit.is(b'"')) {
            return Err(self.unexpected(rest, expected));
        }
        let (node, length) = self.with_characters(rest, |tables, text| tables.follow(parent, text))?;
        let node = self.cx.or_out_of_memory(node)?;
        match skip_white_space(&rest[length..]) {
            [colon, after @ ..] if colon.is(b':') => Ok((rest.len() - after.len(), node)),
            after => Err(self.unexpected(after, Expected::Colon)),
        }
    }
}
