use bun_engine_text::{Identifier, PropertyName};

use crate::{Attributes, FastHashSet, Storage};

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DontEnumMode {
    Include,
    Exclude,
}

impl DontEnumMode {
    #[inline(always)]
    pub fn includes(self, attributes: Attributes) -> bool {
        self == DontEnumMode::Include || attributes.is_enumerable()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PropertyNameMode {
    Symbols = 1,
    Strings = 2,
    StringsAndSymbols = 3,
}

/// Collects keys of the kinds that it was asked for, each once.
pub struct PropertyNameList {
    names: Vec<Identifier>,
    seen: FastHashSet<usize>,
    mode: PropertyNameMode,
    includes_private_names: bool,
    has_only_indices: bool,
    has_overflowed: bool,
}

impl PropertyNameList {
    const LINEAR_LIMIT: usize = 20;

    pub fn new(mode: PropertyNameMode) -> PropertyNameList {
        PropertyNameList {
            names: Vec::new(),
            seen: FastHashSet::default(),
            mode,
            includes_private_names: false,
            has_only_indices: true,
            has_overflowed: false,
        }
    }

    /// An empty one that accepts the same names.
    pub fn new_like(&self) -> PropertyNameList {
        PropertyNameList {
            includes_private_names: self.includes_private_names,
            ..PropertyNameList::new(self.mode)
        }
    }

    pub fn including_private_names(mut self) -> PropertyNameList {
        self.includes_private_names = true;
        self
    }

    pub fn mode(&self) -> PropertyNameMode {
        self.mode
    }

    pub fn includes_private_names(&self) -> bool {
        self.includes_private_names
    }

    pub(super) fn has_only_indices(&self) -> bool {
        self.has_only_indices
    }

    /// There was no memory for all of them.
    pub(super) fn has_overflowed(&self) -> bool {
        self.has_overflowed
    }

    #[inline]
    fn push(&mut self, name: PropertyName<'_>) {
        self.has_only_indices = self.has_only_indices && name.as_index().is_some();
        self.names.push(name.to_identifier());
    }

    pub fn includes_strings(&self) -> bool {
        self.mode != PropertyNameMode::Symbols
    }

    pub fn includes_symbols(&self) -> bool {
        self.mode != PropertyNameMode::Strings
    }

    #[inline]
    fn accepts(&self, name: PropertyName<'_>) -> bool {
        match name.symbol_flags() {
            None => self.includes_strings(),
            Some(flags) => {
                self.includes_symbols() && (self.includes_private_names || !flags.is_private())
            }
        }
    }

    pub fn add(&mut self, name: PropertyName<'_>) {
        if !self.accepts(name) {
            return;
        }
        let is_new = if self.names.len() < Self::LINEAR_LIMIT {
            !self.names.iter().any(|it| *it == name)
        } else {
            let unseen = &self.names[self.seen.len()..];
            self.seen
                .extend(unseen.iter().map(|it| it.name().address()));
            self.seen.insert(name.address())
        };
        if is_new {
            self.push(name);
        }
    }

    /// The list does not have the name.
    #[inline]
    pub fn add_known_unique(&mut self, name: PropertyName<'_>) {
        if self.accepts(name) {
            self.push(name);
        }
    }

    pub fn add_index(&mut self, index: u32) {
        if self.includes_strings() {
            self.add(Identifier::from_index(index).name());
        }
    }

    /// In ascending order, and none twice.
    pub fn add_indices(&mut self, indices: impl Iterator<Item = u32>) {
        if !self.includes_strings() {
            return;
        }
        let count = indices.size_hint().0;
        if count > Storage::MAX_VECTOR_LENGTH as usize || self.names.try_reserve(count).is_err() {
            self.has_overflowed = true;
        } else if self.is_empty() {
            self.names.extend(indices.map(Identifier::from_index));
        } else {
            indices.for_each(|index| self.add_index(index));
        }
    }

    pub fn into_vec(self) -> Vec<Identifier> {
        self.names
    }
}

impl core::ops::Deref for PropertyNameList {
    type Target = [Identifier];

    fn deref(&self) -> &[Identifier] {
        &self.names
    }
}
