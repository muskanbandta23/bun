//! Checks that code is well formed: it decodes, and all that it names exists and is of the right kind.

use bun_engine_syntax::EvalContext;
use bun_engine_text::Identifier;

use crate::code::{Body, Code, CodeFlags, IntCases};
use crate::constant::{ArrayLiteral, Constant, LinkTimeConstant};
use crate::graph::refill;
use crate::instruction::{Flow, Opcode, RawInstruction, Shape};
use crate::metadata::MetadataKind;
use crate::operand::*;
use crate::scope::ScopeKind;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
    TooLarge,
    Header,
    MetadataLayout,
    Positions,
    Handlers,
    Switches,
    SuspendPoints,
    SavedRegisters,
    ScopeRegisters,
    Constants,
    NameLists,
    NotAnInstruction,
    /// The instruction may not stand where it does.
    Order,
    FallsOffTheEnd,
    /// The instruction is not for code with these flags.
    Flags,
    Metadata,
    /// By its index.
    Operand(u8),
    /// The offset is that of the target.
    Target,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VerifyError {
    pub fault: Fault,
    /// Of the instruction, if it is about one.
    pub offset: Option<u32>,
}

impl From<Fault> for VerifyError {
    fn from(fault: Fault) -> VerifyError {
        VerifyError {
            fault,
            offset: None,
        }
    }
}

struct Bits(Vec<u64>);

impl Bits {
    fn new(len: usize) -> Result<Bits, Fault> {
        let mut words = Vec::new();
        refill(&mut words, len.div_ceil(64), 0).map_err(|_| Fault::TooLarge)?;
        Ok(Bits(words))
    }

    #[inline]
    fn has(&self, index: u32) -> bool {
        let word = self.0.get(index as usize / 64);
        word.is_some_and(|word| word >> (index % 64) & 1 != 0)
    }

    #[inline]
    fn add(&mut self, index: u32) {
        self.0[index as usize / 64] |= 1 << (index % 64);
    }
}

const fn required_flags(opcode: Opcode) -> CodeFlags {
    match opcode {
        Opcode::ResumeDispatch
        | Opcode::CreateGenerator
        | Opcode::CreateAsyncGenerator
        | Opcode::Suspend
        | Opcode::Await
        | Opcode::Resume
        | Opcode::ResumeAfterYield
        | Opcode::ResumeAfterAwait
        | Opcode::ResumeAfterDelegate
        | Opcode::GeneratorReturn
        | Opcode::GeneratorThrow
        | Opcode::AsyncIteratorNext => CodeFlags::SUSPENDS,
        Opcode::GetInternalField
        | Opcode::PutInternalField
        | Opcode::GetByIdDirect
        | Opcode::ResumeGenerator
        | Opcode::HasShapeWithFlags
        | Opcode::NewPromise
        | Opcode::CreatePromise
        | Opcode::NewArrayWithSpecies
        | Opcode::ToObject => CodeFlags::BUILTIN,
        Opcode::TailCall | Opcode::TailCallVarargs => CodeFlags::HAS_TAIL_CALLS,
        Opcode::Debug | Opcode::DebugAwait | Opcode::JumpIfFrameIsKept => {
            CodeFlags::HAS_DEBUG_HOOKS
        }
        Opcode::Cover => CodeFlags::HAS_COVERAGE,
        _ => CodeFlags::empty(),
    }
}

fn is_primitive(constant: &Constant) -> bool {
    matches!(
        constant,
        Constant::Undefined
            | Constant::Null
            | Constant::Boolean(_)
            | Constant::Int32(_)
            | Constant::Double(_)
            | Constant::String(_)
            | Constant::LongString(_)
            | Constant::BigInt(_)
    )
}

fn is_valid_constant(constant: &Constant, flags: CodeFlags) -> bool {
    match constant {
        Constant::BigInt(literal) => matches!(literal.radix, 2 | 8 | 10 | 16),
        Constant::Template(site) => site.cooked.len() == site.raw.len(),
        Constant::Array(literal) => match &**literal {
            ArrayLiteral::Values(values) => values
                .iter()
                .all(|value| is_primitive(value) && is_valid_constant(value, flags)),
            _ => true,
        },
        Constant::LinkTime(constant) => {
            constant.0 < LinkTimeConstant::FIRST_OF_HOST.0 || flags.contains(CodeFlags::BUILTIN)
        }
        _ => true,
    }
}

fn are_distinct_and_no_indices(names: &[Identifier], scratch: &mut Vec<usize>) -> bool {
    scratch.clear();
    scratch.extend(names.iter().map(|name| name.name().address()));
    scratch.sort_unstable();
    scratch.is_sorted_by(|a, b| a < b) && names.iter().all(|name| name.name().as_index().is_none())
}

struct Verifier<'a> {
    code: &'a Code,
    body: &'a Body,
    starts: Bits,
    targets: Bits,
    used_int_switches: Bits,
    used_string_switches: Bits,
    literal_name_lists: Bits,
    /// For each kind, where its next entry starts and where its last ends.
    metadata: [(u32, u32); MetadataKind::COUNT],
    suspend_points: u32,
}

/// Does not look into the code of inner functions.
pub fn verify(code: &Code) -> Result<(), VerifyError> {
    let body = &*code.body;
    let (layout, positions) = body.sections().ok_or(Fault::Header)?;
    let size = body.instructions_size;
    let is_valid = size <= Body::MAX_SIZE
        && (2..=Body::MAX_FRAME_WORDS).contains(&body.locals)
        && body.locals.is_multiple_of(2)
        && (1..=Body::MAX_FRAME_WORDS).contains(&body.parameters)
        && CodeFlags::all().contains(body.flags)
        && (body.parameters == 1 || !body.flags.contains(CodeFlags::SUSPENDS));
    is_valid.ok_or(Fault::Header)?;
    layout.is_valid().ok_or(Fault::MetadataLayout)?;

    let mut verifier = Verifier {
        code,
        body,
        starts: Bits::new(size as usize)?,
        targets: Bits::new(size as usize)?,
        used_int_switches: Bits::new(body.int_switches().len())?,
        used_string_switches: Bits::new(body.string_switches().len())?,
        literal_name_lists: Bits::new(code.name_lists.len())?,
        metadata: [(0, 0); MetadataKind::COUNT],
        suspend_points: 0,
    };
    for (kind, range) in layout.ranges() {
        verifier.metadata[kind as usize] = (range.start, range.end);
    }
    verifier.check_tables()?;
    verifier.check_instructions()?;
    verifier.check_targets()?;
    verifier.check_handlers()?;
    verifier.check_literal_names()?;
    let is_start = |offset| verifier.starts.has(offset);
    positions.is_valid(is_start).ok_or(Fault::Positions)?;
    Ok(())
}

impl Verifier<'_> {
    fn check_tables(&self) -> Result<(), Fault> {
        let (code, body) = (self.code, self.body);
        let mut constants = code.constants.iter();
        constants
            .all(|constant| is_valid_constant(constant, body.flags))
            .ok_or(Fault::Constants)?;

        let keys = code.string_switches.iter().map(|keys| keys.len());
        let targets = body.string_switches().iter();
        keys.eq(targets.map(|switch| switch.targets.len()))
            .ok_or(Fault::Switches)?;
        let mut int_switches = body.int_switches().iter();
        int_switches
            .all(|switch| match &switch.cases {
                IntCases::Dense { first, targets } => {
                    i64::from(*first) + targets.len() as i64 <= i64::from(i32::MAX) + 1
                }
                IntCases::Sparse(cases) => cases.is_sorted_by(|a, b| a.0 < b.0),
            })
            .ok_or(Fault::Switches)?;

        // A point saves nothing, what the last that saves anything does, or the next registers of the table.
        let saved = body
            .tables
            .as_ref()
            .map_or(&[][..], |tables| &tables.saved_registers);
        let mut last = 0..0;
        for point in body.suspend_points() {
            let range = &point.registers;
            if *range == (0..0) || *range == last {
                continue;
            }
            let registers = saved.get(range.start as usize..range.end as usize);
            let is_valid = range.start == last.end
                && registers.is_some_and(|registers| {
                    registers.is_sorted_by(|a, b| a > b)
                        && registers.iter().all(|&register| self.is_run(register, 1))
                });
            is_valid.ok_or(Fault::SavedRegisters)?;
            last = range.clone();
        }
        (last.end as usize == saved.len()).ok_or(Fault::SavedRegisters)?;

        let scopes = (body.tables.as_ref()).map_or(&[][..], |tables| &tables.scope_registers);
        (scopes.is_sorted_by_key(|it| it.0) && scopes.iter().all(|it| self.is_run(it.1, 1)))
            .ok_or(Fault::ScopeRegisters)
    }

    fn check_instructions(&mut self) -> Result<(), VerifyError> {
        let code = self.body.instructions();
        let mut offset = 0;
        let mut previous = None;
        while (offset as usize) < code.len() {
            let at_offset = |fault| VerifyError {
                fault,
                offset: Some(offset),
            };
            let instruction = RawInstruction::read(code, offset)
                .ok_or_else(|| at_offset(Fault::NotAnInstruction))?;
            self.check_instruction(offset, &instruction, previous.as_ref())
                .map_err(at_offset)?;
            self.starts.add(offset);
            offset += u32::from(instruction.size);
            previous = Some(instruction);
        }
        previous
            .is_some_and(|last| matches!(last.opcode.flow(), Flow::Jumps | Flow::Ends))
            .ok_or(Fault::FallsOffTheEnd)?;
        (self.suspend_points as usize == self.body.suspend_points().len())
            .ok_or(Fault::SuspendPoints)?;
        let mut metadata = self.metadata.iter();
        metadata
            .all(|(next, end)| next == end)
            .ok_or(Fault::MetadataLayout)?;
        Ok(())
    }

    #[inline]
    fn check_instruction(
        &mut self,
        offset: u32,
        instruction: &RawInstruction,
        previous: Option<&RawInstruction>,
    ) -> Result<(), Fault> {
        let opcode = instruction.opcode;
        let flags = self.body.flags;
        flags.contains(required_flags(opcode)).ok_or(Fault::Flags)?;
        self.may_follow(instruction, previous).ok_or(Fault::Order)?;
        if let Some(kind) = opcode.metadata() {
            let (next, end) = &mut self.metadata[kind as usize];
            (instruction.metadata == *next && next < end).ok_or(Fault::Metadata)?;
            *next += kind.words();
        }
        for (index, &shape) in opcode.shapes().iter().enumerate() {
            self.is_valid_operand(offset, instruction, index, shape)
                .ok_or(Fault::Operand(index as u8))?;
        }
        if opcode.branches() && opcode != Opcode::ResumeDispatch {
            let mut fault = Ok(());
            instruction.for_each_target(self.body, offset, |target| {
                if target < self.body.instructions_size {
                    self.targets.add(target);
                } else {
                    fault = Err(Fault::Target);
                }
            });
            fault?;
        }
        Ok(())
    }

    #[inline]
    fn may_follow(&self, instruction: &RawInstruction, previous: Option<&RawInstruction>) -> bool {
        let opcode = instruction.opcode;
        let Some(previous) = previous else {
            return opcode == Opcode::Enter;
        };
        match previous.opcode {
            Opcode::Enter if self.body.flags.contains(CodeFlags::SUSPENDS) => {
                opcode == Opcode::ResumeDispatch
            }
            Opcode::TailCall | Opcode::TailCallVarargs => {
                opcode == Opcode::Return && instruction.words()[0] == previous.words()[0]
            }
            Opcode::Suspend | Opcode::Await => opcode == Opcode::Resume,
            _ => match opcode {
                Opcode::Enter | Opcode::ResumeDispatch | Opcode::Resume => false,
                Opcode::Catch => !previous.opcode.flow().falls_through(),
                _ => true,
            },
        }
    }

    #[inline]
    fn is_writable(&self, register: Register) -> bool {
        match register.local_index() {
            Some(local) => local < self.body.locals,
            None => {
                let parameter = register.offset().wrapping_sub(Register::THIS.offset());
                (parameter as u32) < self.body.parameters
            }
        }
    }

    #[inline]
    fn is_readable(&self, register: Register) -> bool {
        self.is_writable(register) || register == Register::CALLEE
    }

    fn is_run(&self, first: Register, count: u32) -> bool {
        first
            .local_index()
            .is_some_and(|local| u64::from(local) + u64::from(count) <= u64::from(self.body.locals))
    }

    /// `arguments` counts `this`.
    fn is_frame(&self, frame: u32, arguments: u32) -> bool {
        frame.is_multiple_of(2)
            && frame <= self.body.locals
            && u64::from(Register::HEADER_WORDS) + u64::from(arguments) <= u64::from(frame)
    }

    #[inline]
    fn is_valid_operand(
        &mut self,
        offset: u32,
        instruction: &RawInstruction,
        index: usize,
        shape: Shape,
    ) -> bool {
        let (code, body) = (self.code, self.body);
        let (opcode, words) = (instruction.opcode, instruction.words());
        let word = words[index];
        let register = Register::from_word(word);
        let is_index = |len: usize| (word as usize) < len;
        let is_subset = |bits: u8| word & !u32::from(bits) == 0;
        match shape {
            Shape::Dst | Shape::DstIfJumps | Shape::DstUnlessJumps => self.is_writable(register),
            Shape::Mut => {
                let has_old = matches!(opcode, Opcode::PostIncrement | Opcode::PostDecrement);
                self.is_writable(register) && !(has_old && word == words[0])
            }
            Shape::Reg => self.is_readable(register),
            Shape::Src => match Operand::from_word(word).as_constant() {
                None => self.is_readable(register),
                Some(constant) => match code.constants.get(constant as usize) {
                    None | Some(Constant::Array(_) | Constant::RegExp(_)) => false,
                    Some(Constant::String(_) | Constant::LongString(_)) => true,
                    Some(_) => opcode != Opcode::ThrowError,
                },
            },
            Shape::Dst2 | Shape::Reg2 => self.is_run(register, 2),
            Shape::Dst3 | Shape::Reg3 | Shape::Mut3 => self.is_run(register, 3),
            Shape::First => words[index + 1] == 0 || self.is_run(register, words[index + 1]),
            Shape::Count | Shape::Target => true,
            Shape::ArgumentCount => word != 0,
            Shape::Frame => self.is_frame(word, words[index - 1]),
            Shape::Frame1 => self.is_frame(word, 2),
            Shape::Frame2 => self.is_frame(word, 3),
            Shape::Unsigned => word <= Body::MAX_FRAME_WORDS || opcode == Opcode::HasShapeWithFlags,
            Shape::Variables => word < body.locals,
            Shape::Parameter => word < body.parameters - 1,
            Shape::Flag => word <= 1,
            Shape::Name => is_index(code.identifiers.len()),
            Shape::Names => code.name_lists.get(word as usize).is_some_and(|names| {
                if opcode != Opcode::NewObjectLiteral {
                    return true;
                }
                self.literal_name_lists.add(word);
                names.len() == words[index + 2] as usize
            }),
            Shape::Function => code.functions.get(word as usize).is_some_and(|function| {
                function.kind().is_class_constructor() == (opcode == Opcode::NewClass)
            }),
            Shape::Scope => code.scopes.get(word as usize).is_some_and(|scope| {
                (scope.kind() == ScopeKind::With) == (opcode == Opcode::CreateWithScope)
            }),
            Shape::ArrayConstant => {
                matches!(code.constants.get(word as usize), Some(Constant::Array(_)))
            }
            Shape::RegExpConstant => {
                matches!(code.constants.get(word as usize), Some(Constant::RegExp(_)))
            }
            Shape::IntSwitch => {
                is_index(body.int_switches().len()) && use_once(&mut self.used_int_switches, word)
            }
            Shape::StringSwitch => {
                is_index(body.string_switches().len())
                    && use_once(&mut self.used_string_switches, word)
            }
            Shape::SpreadMask => is_index(body.spread_masks().len()),
            Shape::SuspendPoint => self.is_next_suspend_point(offset, instruction, word),
            Shape::CoverageBlock => is_index(body.coverage_blocks().len()),
            Shape::Unresolved => word < Unresolved::COUNT,
            Shape::ErrorKind => word < ErrorKind::COUNT,
            Shape::StaticError => word < StaticError::COUNT,
            Shape::SuspendKind => word < SuspendKind::COUNT,
            Shape::CloseMode => word < CloseMode::COUNT,
            Shape::ImportPhase => word < ImportPhase::COUNT,
            Shape::DebugHook => word < DebugHook::COUNT,
            Shape::ArrayShape => word < ArrayShape::COUNT,
            Shape::PutFlags => is_subset(PutFlags::all().bits()),
            Shape::DefineFlags => is_subset(DefineFlags::all().bits()),
            Shape::CellType => is_subset(u8::MAX),
            Shape::Context => is_subset(EvalContext::all().bits()),
        }
    }

    /// The points are in the order of the code.
    fn is_next_suspend_point(
        &mut self,
        offset: u32,
        instruction: &RawInstruction,
        index: u32,
    ) -> bool {
        let Some(point) = self.body.suspend_points().get(index as usize) else {
            return false;
        };
        if index != self.suspend_points {
            return false;
        }
        match instruction.opcode {
            Opcode::Resume => {
                self.suspend_points += 1;
                point.resume == offset
            }
            Opcode::Await => point.suspend == offset && point.kind == SuspendKind::Await,
            _ => point.suspend == offset && point.kind.word() == instruction.words()[2],
        }
    }

    fn opcode_at(&self, offset: u32) -> Option<Opcode> {
        let instruction = Opcode::read(self.body.instructions(), offset);
        instruction
            .filter(|_| self.starts.has(offset))
            .map(|(opcode, _)| opcode)
    }

    fn check_targets(&self) -> Result<(), VerifyError> {
        for (word, &(mut bits)) in (0..).zip(&self.targets.0) {
            while bits != 0 {
                let target = word * 64 + bits.trailing_zeros();
                if let None
                | Some(
                    Opcode::Enter | Opcode::ResumeDispatch | Opcode::Catch | Opcode::Resume,
                ) = self.opcode_at(target)
                {
                    return Err(VerifyError {
                        fault: Fault::Target,
                        offset: Some(target),
                    });
                }
                bits &= bits - 1;
            }
        }
        Ok(())
    }

    fn check_handlers(&self) -> Result<(), Fault> {
        let mut handlers = self.body.handlers().iter();
        handlers
            .all(|handler| {
                let range = &handler.range;
                range.start < range.end
                    && self.starts.has(range.start)
                    && (self.starts.has(range.end) || range.end == self.body.instructions_size)
                    && self.opcode_at(handler.target) == Some(Opcode::Catch)
            })
            .ok_or(Fault::Handlers)
    }

    fn check_literal_names(&self) -> Result<(), Fault> {
        let mut scratch = Vec::new();
        (0..)
            .zip(&self.code.name_lists)
            .filter(|&(index, _)| self.literal_name_lists.has(index))
            .all(|(_, names)| are_distinct_and_no_indices(names, &mut scratch))
            .ok_or(Fault::NameLists)
    }
}

fn use_once(used: &mut Bits, index: u32) -> bool {
    let is_first = !used.has(index);
    used.add(index);
    is_first
}
