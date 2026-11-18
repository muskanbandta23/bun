//! The iteration protocol, the iterators that the tiers have fast paths for, and the lists that are made with them.

mod allocation_profile;
mod argument_list;
mod array_iterator;
mod helpers;
mod join;
mod mode;
mod new_array;
mod protocol;
mod record;
mod spread;
mod string_iterator;

pub use allocation_profile::ArrayAllocationProfile;
pub use argument_list::{
    argument_list_length, arguments_from_array_like, load_argument_list, load_spread_arguments,
    to_argument_list,
};
pub use array_iterator::{IterationKind, JsArrayIterator};
pub use helpers::{JsIterator, JsIteratorHelper, JsWrapForValidIterator};
pub use mode::IterationMode;
pub use new_array::{
    append_to_array, array_species_create, new_array, new_array_from_constant, new_array_with_size,
    new_array_with_species, new_array_with_spread, spread_length,
};
pub use protocol::{
    ITERATOR_RESULT_DONE_OFFSET, ITERATOR_RESULT_VALUE_OFFSET, iterator_close,
    iterator_close_on_throw, iterator_close_quietly, iterator_complete, iterator_method,
    iterator_next, iterator_value, throw_not_iterable,
};
pub use record::{IteratorRecord, for_each_in_iterable};
pub use spread::{spread, try_spread_directly};
pub use string_iterator::JsStringIterator;

#[derive(Default)]
pub(crate) struct State {
    being_joined: join::InProgress,
}

impl crate::services::Service for State {}
