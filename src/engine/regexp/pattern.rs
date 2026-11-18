use bun_engine_text::Text;

use crate::construct::{GroupNames, build};
use crate::tree::Tree;
use crate::{Error, Flags};

/// A parsed pattern.
pub struct Pattern {
    tree: Tree,
    names: GroupNames,
}

impl Pattern {
    pub fn new(source: Text<'_>, flags: Flags) -> Result<Pattern, Error> {
        let (tree, names, _) = build(source, flags)?;
        Ok(Pattern { tree, names })
    }

    #[inline]
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// The whole match does not count.
    #[inline]
    pub fn group_count(&self) -> u16 {
        self.tree.group_count()
    }

    #[inline]
    pub fn group_names(&self) -> &GroupNames {
        &self.names
    }
}
