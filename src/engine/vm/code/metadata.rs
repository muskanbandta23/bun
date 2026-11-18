//! What the entries of the metadata of a code block look like.

use crate::heap::Survivors;
use crate::{CodeBlock, Visitor};

#[derive(Default)]
pub(crate) struct SideTables {}

pub(super) fn visit(_: &CodeBlock, _: &mut Visitor) {}

pub(super) fn reconcile_weak_references(_: &CodeBlock, _: &Survivors) {}
