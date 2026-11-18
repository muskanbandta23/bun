//! Looks for the members of a class in a long text.

use bun_engine_text::CodeUnit;

use crate::program::Class;

#[inline]
pub(crate) fn find_member<U: CodeUnit>(units: &[U], class: &Class) -> Option<usize> {
    units.iter().position(|unit| class.contains(unit.to_u32()))
}
