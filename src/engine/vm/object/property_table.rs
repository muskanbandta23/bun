use bun_engine_macros::FieldOffsets;
use bun_engine_text::{Identifier, PropertyName};

use crate::{Attributes, PropertyOffset};

#[derive(Clone, FieldOffsets)]
#[repr(C)]
pub struct PropertyEntry {
    /// `None` once the property has been removed.
    key: Option<Identifier>,
    offset: PropertyOffset,
    attributes: Attributes,
}

/// Up to this many entries are searched in order.
const LINEAR_LIMIT: usize = 8;

/// The properties of a shape, in the order in which they were added.
#[derive(Clone, Default)]
pub(crate) struct PropertyTable {
    entries: Vec<PropertyEntry>,
    /// Linear probing. A slot is an index into `entries` plus one, or zero.
    index: Box<[u32]>,
    live: u32,
    deleted_offsets: Vec<PropertyOffset>,
}

impl PropertyTable {
    pub(crate) fn clone_with_capacity(&self, capacity: usize) -> PropertyTable {
        let mut entries = Vec::with_capacity(capacity.max(self.entries.len()));
        entries.extend_from_slice(&self.entries);
        PropertyTable {
            entries,
            index: self.index.clone(),
            live: self.live,
            deleted_offsets: self.deleted_offsets.clone(),
        }
    }

    pub(crate) fn with_capacity(capacity: usize) -> PropertyTable {
        PropertyTable {
            entries: Vec::with_capacity(capacity),
            ..PropertyTable::default()
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.live as usize
    }

    pub(crate) fn memory_size(&self) -> usize {
        size_of::<PropertyTable>()
            + self.entries.capacity() * size_of::<PropertyEntry>()
            + size_of_val(&*self.index)
    }

    #[inline]
    fn position(&self, name: PropertyName<'_>) -> Option<usize> {
        let has_name = |entry: &PropertyEntry| entry.key.as_ref().is_some_and(|key| *key == name);
        if self.index.is_empty() {
            return self.entries.iter().position(has_name);
        }
        let mask = self.index.len() - 1;
        let mut slot = name.hash() as usize & mask;
        loop {
            let position = (self.index[slot] as usize).checked_sub(1)?;
            if has_name(&self.entries[position]) {
                return Some(position);
            }
            slot = (slot + 1) & mask;
        }
    }

    #[inline]
    pub(crate) fn get(&self, name: PropertyName<'_>) -> Option<(PropertyOffset, Attributes)> {
        let entry = &self.entries[self.position(name)?];
        Some((entry.offset, entry.attributes))
    }

    /// The offset that `add` gives to the next property.
    #[inline]
    pub(crate) fn next_offset(&self, inline_capacity: usize) -> PropertyOffset {
        match self.deleted_offsets.last() {
            Some(&offset) => offset,
            None => PropertyOffset::for_slot(self.len(), inline_capacity),
        }
    }

    /// The table does not have the name.
    #[inline]
    pub(crate) fn add(
        &mut self,
        name: PropertyName<'_>,
        attributes: Attributes,
        inline_capacity: usize,
    ) -> PropertyOffset {
        debug_assert!(self.position(name).is_none());
        let offset = self.next_offset(inline_capacity);
        self.deleted_offsets.pop();
        self.entries.push(PropertyEntry {
            key: Some(name.to_identifier()),
            offset,
            attributes,
        });
        self.live += 1;
        if self.entries.len() * 2 > self.index.len() {
            if self.entries.len() > LINEAR_LIMIT {
                self.rebuild();
            }
        } else {
            Self::insert(&mut self.index, name, self.entries.len() as u32);
        }
        offset
    }

    fn insert(index: &mut [u32], name: PropertyName<'_>, value: u32) {
        let mask = index.len() - 1;
        let mut slot = name.hash() as usize & mask;
        while index[slot] != 0 {
            slot = (slot + 1) & mask;
        }
        index[slot] = value;
    }

    #[cold]
    fn rebuild(&mut self) {
        self.entries.retain(|entry| entry.key.is_some());
        if self.entries.len() <= LINEAR_LIMIT {
            self.index = Box::default();
            return;
        }
        let capacity = (self.entries.len() * 3).next_power_of_two();
        self.index = vec![0; capacity].into_boxed_slice();
        for (position, entry) in self.entries.iter().enumerate() {
            if let Some(key) = &entry.key {
                Self::insert(&mut self.index, key.name(), position as u32 + 1);
            }
        }
    }

    pub(crate) fn remove(&mut self, name: PropertyName<'_>) -> Option<PropertyOffset> {
        let position = self.position(name)?;
        let offset = self.entries[position].offset;
        if self.index.is_empty() {
            self.entries.remove(position);
        } else {
            self.entries[position].key = None;
        }
        self.live -= 1;
        self.deleted_offsets.push(offset);
        if self.entries.len() > LINEAR_LIMIT && self.len() * 4 < self.entries.len() {
            self.rebuild();
        }
        Some(offset)
    }

    pub(crate) fn set_attributes(
        &mut self,
        name: PropertyName<'_>,
        attributes: Attributes,
    ) -> Option<PropertyOffset> {
        let position = self.position(name)?;
        self.entries[position].attributes = attributes;
        Some(self.entries[position].offset)
    }

    pub(crate) fn iter(
        &self,
    ) -> impl Iterator<Item = (PropertyName<'_>, PropertyOffset, Attributes)> {
        self.entries
            .iter()
            .filter_map(|entry| Some((entry.key.as_ref()?.name(), entry.offset, entry.attributes)))
    }

    pub(crate) fn entries(&self) -> &[PropertyEntry] {
        &self.entries
    }

    /// Of all properties but private elements.
    pub(crate) fn update_attributes(&mut self, update: impl Fn(Attributes) -> Attributes) {
        for entry in &mut self.entries {
            if (entry.key.as_ref()).is_some_and(|key| !key.name().is_private_element()) {
                entry.attributes = update(entry.attributes);
            }
        }
    }

    /// In that order. The others follow in theirs.
    pub(crate) fn move_to_front(&mut self, names: impl Iterator<Item = Identifier>) {
        let mut rank = vec![usize::MAX; self.entries.len()];
        for (number, name) in names.enumerate() {
            if let Some(position) = self.position(name.name()) {
                rank[position] = rank[position].min(number);
            }
        }
        let mut ranked: Vec<_> = rank.into_iter().zip(self.entries.drain(..)).collect();
        ranked.sort_by_key(|it| it.0);
        self.entries.extend(ranked.into_iter().map(|it| it.1));
        self.rebuild();
    }

    /// Closes the gaps between the offsets, and returns the old ones.
    pub(crate) fn renumber(&mut self, inline_capacity: usize) -> Vec<PropertyOffset> {
        self.deleted_offsets = Vec::new();
        self.entries
            .iter_mut()
            .filter(|entry| entry.key.is_some())
            .enumerate()
            .map(|(number, entry)| {
                let offset = PropertyOffset::for_slot(number, inline_capacity);
                core::mem::replace(&mut entry.offset, offset)
            })
            .collect()
    }
}
