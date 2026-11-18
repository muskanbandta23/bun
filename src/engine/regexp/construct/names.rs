use bun_engine_text::Text;

use crate::Error;
use crate::parser::NameId;
use crate::tree::{Group, Groups, Node, NodeId, Tree};

/// The names of groups, in the order of the first group that has each.
#[derive(Default)]
pub struct GroupNames {
    units: Vec<u16>,
    groups: Vec<Group>,
    /// In `units` and in `groups`.
    ends: Vec<(u32, u32)>,
}

impl GroupNames {
    #[inline]
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Each with its groups, ascending. At most one of them takes part in a match.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (Text<'_>, &[Group])> {
        let mut start = (0, 0);
        self.ends.iter().map(move |&end| {
            let (units, groups) = core::mem::replace(&mut start, end);
            (
                Text::Utf16(&self.units[units as usize..end.0 as usize]),
                &self.groups[groups as usize..end.1 as usize],
            )
        })
    }
}

#[derive(Default)]
pub(super) struct NamedGroups {
    names: GroupNames,
    /// For each name that a group has by now, its position among `names`.
    positions: Vec<Option<u16>>,
    /// With the position of the name.
    groups: Vec<(u16, Group)>,
    references: Vec<(NodeId, NameId, Groups)>,
}

impl NamedGroups {
    pub(super) fn add_group(
        &mut self,
        group: Group,
        name: NameId,
        text: Text<'_>,
    ) -> Result<(), Error> {
        let name = name as usize;
        if name >= self.positions.len() {
            self.positions.resize(name + 1, None);
        }
        let position = match self.positions[name] {
            Some(position) => position,
            None => {
                let GroupNames { units, ends, .. } = &mut self.names;
                units
                    .try_reserve(text.len())
                    .map_err(|_| Error::OutOfMemory)?;
                units.extend(text.code_units());
                ends.push((units.len() as u32, 0));
                *self.positions[name].insert(ends.len() as u16 - 1)
            }
        };
        self.groups.push((position, group));
        Ok(())
    }

    pub(super) fn add_reference(
        &mut self,
        node: NodeId,
        name: NameId,
        candidates: Groups,
    ) -> Result<(), Error> {
        self.references
            .try_reserve(1)
            .map_err(|_| Error::OutOfMemory)?;
        self.references.push((node, name, candidates));
        Ok(())
    }

    #[inline]
    pub(super) fn forget_references_from(&mut self, first_node: usize) {
        let is_gone = |&mut (node, ..): &mut (NodeId, _, _)| node.index() >= first_node;
        while self.references.pop_if(is_gone).is_some() {}
    }

    /// Every name has a group.
    #[inline]
    pub(super) fn resolve(self, tree: &mut Tree) -> Result<GroupNames, Error> {
        match self.groups.is_empty() {
            true => Ok(self.names),
            false => self.resolve_names(tree),
        }
    }

    fn resolve_names(mut self, tree: &mut Tree) -> Result<GroupNames, Error> {
        let mut names = self.names;
        self.groups.sort_unstable();
        for (index, &(position, _)) in self.groups.iter().enumerate() {
            names.ends[usize::from(position)].1 = index as u32 + 1;
        }
        names.groups = self.groups.iter().map(|&(_, group)| group).collect();
        if self.references.is_empty() {
            return Ok(names);
        }
        let all = tree.add_groups(&names.groups)?;
        for (node, name, candidates) in self.references {
            let (Some(&Some(position)), Node::BackReference { ignore_case, .. }) =
                (self.positions.get(name as usize), tree[node])
            else {
                return Err(Error::InvalidNamedReference);
            };
            let before = |group| self.groups.partition_point(|&it| it < (position, group));
            let groups = all.slice(before(candidates.first)..before(candidates.end));
            tree.replace(
                node,
                Node::BackReference {
                    groups,
                    ignore_case,
                },
            );
        }
        Ok(names)
    }
}
