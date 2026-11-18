//! Objects, their shapes and their properties.

mod accessor;
mod array;
mod class;
mod condition;
mod define;
mod descriptor;
mod elements;
mod final_object;
mod get;
mod immutable_array;
mod indexing_mode;
mod inherited;
mod integrity;
mod js_object;
mod lazy;
mod property_key;
mod property_name_list;
mod property_offset;
mod property_table;
mod put;
mod reify;
mod shape;
mod shape_cache;
mod slot;
mod sparse_elements;
mod special_property_cache;
mod static_table;
mod storage;
mod transition_table;
mod visit;

pub use accessor::{CustomGetterSetter, GetterSetter};
pub use array::{JsArray, JsArrayInit};
pub use class::{AsciiLiteral, Class, ClassOps};
pub use condition::{
    ConditionKind, ConditionSet, PropertyCondition, Watchability, watch_condition,
};
pub use define::validate_property_descriptor;
pub use descriptor::PropertyDescriptor;
pub use elements::{Elements, ElementsMut, Int32Elements, PutDirectIndexMode};
pub use final_object::JsFinalObject;
pub use immutable_array::ImmutableArray;
pub use indexing_mode::{ElementKind, IndexingMode};
pub use inherited::Inherited;
pub use integrity::IntegrityLevel;
pub use js_object::{JsDestructibleObject, JsObject, JsObjectInit};
pub use lazy::{LazyClass, LazyField};
pub use property_key::PropertyKey;
pub use property_name_list::{DontEnumMode, PropertyNameList, PropertyNameMode};
pub use property_offset::{Attributes, PropertyOffset};
pub(crate) use reify::function_name;
pub use shape::{CachedNames, Properties, Shape, ShapeFlags, ShapeId, ShapeSpec};
pub(crate) use shape_cache::ShapeCache;
pub use slot::{
    CustomGetter, CustomSetter, DeleteKind, DeletePropertySlot, InternalMethod, PropertySlot,
    PutContext, PutKind, PutPropertySlot,
};
pub use sparse_elements::{SparseElements, SparseEntry};
pub use special_property_cache::SpecialProperty;
pub use static_table::{
    BuiltinGenerator, PropertyCallback, PropertyMaker, StaticIndex, StaticProperty, StaticTable,
};
pub use storage::Storage;
pub use transition_table::TransitionKind;
pub(crate) use visit::{destroy_by_class_field, destroy_by_shape, visit_cell};
