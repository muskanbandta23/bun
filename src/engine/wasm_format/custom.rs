//! Custom sections with a meaning. One that is malformed does not make the module invalid.

use crate::alloc::TryPush;
use crate::index::FuncIndex;
use crate::module::{Module, Span};
use crate::reader::Reader;

fn payloads<'a>(module: &'a Module, name: &'a [u8]) -> impl Iterator<Item = Span> + 'a {
    let sections = module.custom_sections().iter();
    sections
        .filter(move |section| module.bytes(section.name) == name)
        .map(|section| section.payload)
}

/// Names are valid UTF-8.
pub struct Names {
    module: Option<Span>,
    /// By function index. The start is zero if there is no name.
    funcs: Vec<Span>,
}

impl Names {
    const MODULE: u8 = 0;
    const FUNCS: u8 = 1;
    const LOCALS: u8 = 2;

    pub fn module(&self) -> Option<Span> {
        self.module
    }

    pub fn func(&self, func: FuncIndex) -> Option<Span> {
        let name = self.funcs.get(func.usize()).copied();
        name.filter(|name| name.start != 0)
    }

    pub(crate) fn parse(module: &Module) -> Option<Self> {
        payloads(module, b"name").find_map(|payload| Self::parse_section(module, payload))
    }

    fn parse_section(module: &Module, payload: Span) -> Option<Self> {
        let name = |reader: &mut Reader<'_>| {
            let len = reader.u32()?;
            let start = payload.start + reader.position() as u32;
            core::str::from_utf8(reader.slice(len as usize)?).ok()?;
            Some(Span { start, len })
        };
        let mut names = Self {
            module: None,
            funcs: Vec::new(),
        };
        let mut reader = Reader::new(module.bytes(payload), 0);
        while reader.remaining() != 0 {
            let kind = reader.u7()?;
            let size = reader.u32()? as usize;
            let end = reader.position().checked_add(size)?;
            match kind {
                Self::MODULE => names.module = Some(name(&mut reader)?),
                Self::FUNCS => {
                    let count = module.funcs().len();
                    names
                        .funcs
                        .try_reserve_exact(count - names.funcs.len())
                        .ok()?;
                    names.funcs.resize(count, Span::default());
                    for _ in 0..reader.u32()? {
                        let func = reader.u32()?;
                        *names.funcs.get_mut(func as usize)? = name(&mut reader)?;
                    }
                }
                Self::LOCALS => {
                    for _ in 0..reader.u32()? {
                        reader.u32()?;
                        for _ in 0..reader.u32()? {
                            reader.u32()?;
                            name(&mut reader)?;
                        }
                    }
                }
                _ => {
                    reader.slice(size)?;
                }
            }
            if reader.position() != end {
                return None;
            }
        }
        Some(names)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BranchHint {
    pub func: FuncIndex,
    /// Of an `if` or `br_if`, as [`crate::Visit::at`] has it.
    pub offset: u32,
    /// Whether the condition is likely to hold.
    pub likely: bool,
}

pub struct BranchHints {
    /// Sorted by function and offset.
    hints: Vec<BranchHint>,
}

impl BranchHints {
    /// Sorted by offset.
    pub fn of(&self, func: FuncIndex) -> &[BranchHint] {
        let start = self.hints.partition_point(|hint| hint.func < func);
        let count = self.hints[start..].partition_point(|hint| hint.func == func);
        &self.hints[start..][..count]
    }

    /// Takes the first section, up to the function where it is malformed.
    pub(crate) fn parse(module: &Module) -> Self {
        let mut hints = Vec::new();
        if let Some(payload) = payloads(module, b"metadata.code.branch_hint").next() {
            let mut reader = Reader::new(module.bytes(payload), 0);
            let mut complete = 0;
            let mut least_func = 0;
            for _ in 0..reader.u32().unwrap_or(0) {
                match Self::parse_function(&mut reader, least_func, &mut hints) {
                    Some(func) => least_func = u64::from(func) + 1,
                    None => break,
                }
                complete = hints.len();
            }
            hints.truncate(complete);
        }
        Self { hints }
    }

    fn parse_function(
        reader: &mut Reader<'_>,
        least_func: u64,
        hints: &mut Vec<BranchHint>,
    ) -> Option<u32> {
        let func = reader.u32().filter(|&func| u64::from(func) >= least_func)?;
        let mut least_offset = 0;
        for _ in 0..reader.u32()? {
            let offset = reader
                .u32()
                .filter(|&offset| u64::from(offset) >= least_offset)?;
            least_offset = u64::from(offset) + 1;
            reader.u32().filter(|&size| size == 1)?;
            let value = reader.u8().filter(|&value| value <= 1)?;
            let hint = BranchHint {
                func: FuncIndex(func),
                offset,
                likely: value == 1,
            };
            hints.try_push(hint).ok()?;
        }
        Some(func)
    }
}
