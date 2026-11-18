use crate::prelude::*;

static_table!(static PROTOTYPE_PROPERTIES = []);

define_object!(IteratorPrototype, "Iterator", PROTOTYPE_PROPERTIES);

pub(crate) fn initialize_prototype(cx: &Cx) {
    let prototype = IteratorPrototype::create(cx, cx.object_prototype().into());
    cx.global().init_iterator_prototype(prototype);
}
