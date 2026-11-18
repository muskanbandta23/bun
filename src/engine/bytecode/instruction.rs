//! The types that the table of instructions expands to.

use core::ops::Range;

use bun_engine_syntax::EvalContext;

use crate::metadata::MetadataKind;
use crate::operand::*;

/// What an operand means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Shape {
    /// A register that is written.
    Dst,
    DstIfJumps,
    DstUnlessJumps,
    /// A register that is read.
    Reg,
    /// A register that is read and written.
    Mut,
    /// A register or a constant that is read.
    Src,
    Dst2,
    Reg2,
    Dst3,
    Reg3,
    Mut3,
    /// The first of as many locals as the `Count` after it says, which are read.
    First,
    Count,
    /// With `this`. The arguments are in their slots of the `Frame` after it.
    ArgumentCount,
    Frame,
    /// A frame with room for one argument, which the instruction stores.
    Frame1,
    Frame2,
    Target,
    Unsigned,
    Variables,
    Parameter,
    Flag,
    Name,
    Names,
    Function,
    Scope,
    ArrayConstant,
    RegExpConstant,
    IntSwitch,
    StringSwitch,
    SpreadMask,
    SuspendPoint,
    CoverageBlock,
    Unresolved,
    ErrorKind,
    StaticError,
    SuspendKind,
    CloseMode,
    ImportPhase,
    DebugHook,
    ArrayShape,
    PutFlags,
    DefineFlags,
    CellType,
    Context,
}

impl Shape {
    pub const fn is_signed(self) -> bool {
        self as u8 <= Shape::First as u8 || matches!(self, Shape::Target)
    }
}

/// Where control goes after an instruction, besides its targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flow {
    /// To the next instruction.
    Pure,
    /// To the next instruction or to a handler.
    Throws,
    /// Nowhere else.
    Jumps,
    /// Out of the frame or to a handler.
    Ends,
    /// Out of the frame or to a handler. A resumption continues with the next instruction.
    Suspends,
}

impl Flow {
    pub const fn can_throw(self) -> bool {
        matches!(self, Flow::Throws | Flow::Ends | Flow::Suspends)
    }

    pub const fn falls_through(self) -> bool {
        matches!(self, Flow::Pure | Flow::Throws)
    }
}

macro_rules! field_type {
    (Dst) => {
        Register
    };
    (DstIfJumps) => {
        Register
    };
    (DstUnlessJumps) => {
        Register
    };
    (Reg) => {
        Register
    };
    (Mut) => {
        Register
    };
    (Src) => {
        Operand
    };
    (Dst2) => {
        Register
    };
    (Reg2) => {
        Register
    };
    (Dst3) => {
        Register
    };
    (Reg3) => {
        Register
    };
    (Mut3) => {
        Register
    };
    (First) => {
        Register
    };
    (Count) => {
        u32
    };
    (ArgumentCount) => {
        u32
    };
    (Frame) => {
        u32
    };
    (Frame1) => {
        u32
    };
    (Frame2) => {
        u32
    };
    (Target) => {
        Jump
    };
    (Unsigned) => {
        u32
    };
    (Variables) => {
        u32
    };
    (Parameter) => {
        u32
    };
    (Flag) => {
        bool
    };
    (Name) => {
        IdentifierIndex
    };
    (Names) => {
        NameListIndex
    };
    (Function) => {
        FunctionIndex
    };
    (Scope) => {
        ScopeIndex
    };
    (ArrayConstant) => {
        ConstantIndex
    };
    (RegExpConstant) => {
        ConstantIndex
    };
    (IntSwitch) => {
        IntSwitchIndex
    };
    (StringSwitch) => {
        StringSwitchIndex
    };
    (SpreadMask) => {
        SpreadMaskIndex
    };
    (SuspendPoint) => {
        SuspendPointIndex
    };
    (CoverageBlock) => {
        CoverageBlockIndex
    };
    (Context) => {
        EvalContext
    };
    ($other:ident) => {
        $other
    };
}
pub(crate) use field_type;

impl Field for EvalContext {
    #[inline(always)]
    fn read<W: Width>(bytes: impl Bytes, index: usize) -> EvalContext {
        EvalContext::from_bits_retain(W::unsigned(bytes, index) as u8)
    }

    #[inline(always)]
    fn word(self) -> u32 {
        u32::from(self.bits())
    }
}

/// The operands of one kind of instruction.
pub trait Op: Copy {
    const OPCODE: Opcode;

    /// `bytes` starts at the opcode, after the prefix.
    fn decode<W: Width>(bytes: impl Bytes) -> Self;

    /// With the prefix.
    #[inline(always)]
    fn size<W: Width>() -> usize {
        Self::OPCODE.size::<W>()
    }
}

macro_rules! define_instructions {
    ($(
        $(#[$doc:meta])*
        $name:ident $mnemonic:ident $flow:ident { $($field:ident: $shape:ident),* } $(=> $metadata:ident)?;
    )*) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(u8)]
        pub enum Opcode {
            $($name,)*
        }

        impl Opcode {
            pub const ALL: [Opcode; Self::COUNT] = [$(Opcode::$name),*];
            pub const COUNT: usize = [$(Flow::$flow),*].len();
        }

        const DESCRIPTIONS: [Description; Opcode::COUNT] = [$(
            Description {
                flow: Flow::$flow,
                shapes: &[$(Shape::$shape),*],
                metadata: define_instructions!(@metadata $($metadata)?),
            },
        )*];

        static NAMES: [(&str, &[&str]); Opcode::COUNT] = [$(
            (stringify!($mnemonic), &[$(stringify!($field)),*]),
        )*];

        pub mod op {
            use super::*;

            $(
                $(#[$doc])*
                #[derive(Clone, Copy, Debug)]
                pub struct $name {
                    $(pub $field: field_type!($shape),)*
                    $(
                        #[doc = concat!("The word at which its `", stringify!($metadata), "` entry starts.")]
                        pub metadata: u32,
                    )?
                }

                impl Op for $name {
                    const OPCODE: Opcode = Opcode::$name;

                    #[inline(always)]
                    #[allow(unused_assignments, unused_mut, unused_variables)]
                    fn decode<W: Width>(bytes: impl Bytes) -> $name {
                        let mut index = 0;
                        $(
                            let $field = Field::read::<W>(bytes, index);
                            index += 1;
                        )*
                        $name {
                            $($field,)*
                            $(metadata: define_instructions!(@read $metadata, W::metadata(bytes, index)),)?
                        }
                    }
                }
            )*
        }

        #[derive(Clone, Copy, Debug)]
        pub enum Instruction {
            $($name(op::$name),)*
        }

        impl Instruction {
            fn decode<W: Width>(opcode: Opcode, bytes: &[u8]) -> Instruction {
                match opcode {
                    $(Opcode::$name => Instruction::$name(Op::decode::<W>(bytes)),)*
                }
            }
        }
    };
    (@metadata) => { None };
    (@metadata $metadata:ident) => { Some(MetadataKind::$metadata) };
    (@read $metadata:ident, $read:expr) => { $read };
}

crate::instruction_table!(define_instructions);

#[derive(Clone, Copy)]
struct Description {
    flow: Flow,
    shapes: &'static [Shape],
    metadata: Option<MetadataKind>,
}

impl Opcode {
    pub const MAX_OPERANDS: usize = 6;

    #[inline(always)]
    pub fn from_byte(byte: u8) -> Option<Opcode> {
        Self::ALL.get(usize::from(byte)).copied()
    }

    #[inline(always)]
    pub const fn flow(self) -> Flow {
        DESCRIPTIONS[self as usize].flow
    }

    #[inline(always)]
    pub const fn shapes(self) -> &'static [Shape] {
        DESCRIPTIONS[self as usize].shapes
    }

    #[inline(always)]
    pub const fn metadata(self) -> Option<MetadataKind> {
        DESCRIPTIONS[self as usize].metadata
    }

    /// With the prefix.
    #[inline(always)]
    pub const fn size<W: Width>(self) -> usize {
        let metadata = if self.metadata().is_some() {
            W::METADATA_BYTES
        } else {
            0
        };
        W::PREFIX_BYTES + 1 + self.shapes().len() * W::OPERAND_BYTES + metadata
    }

    /// The opcode and the size of the instruction at `offset`, which may end past the code.
    #[inline(always)]
    pub(crate) fn read(code: &[u8], offset: u32) -> Option<(Opcode, u32)> {
        let opcode_at = |offset: u32| Opcode::from_byte(*code.get(offset as usize)?);
        let (opcode, size) = match opcode_at(offset)? {
            Opcode::Wide16Prefix => opcode_at(offset + 1).map(|it| (it, it.size::<Wide16>()))?,
            Opcode::Wide32Prefix => opcode_at(offset + 1).map(|it| (it, it.size::<Wide32>()))?,
            opcode => (opcode, opcode.size::<Narrow>()),
        };
        Some((opcode, size as u32))
    }

    pub fn mnemonic(self) -> &'static str {
        let (mnemonic, _) = NAMES[self as usize];
        mnemonic.strip_prefix("r#").unwrap_or(mnemonic)
    }

    pub fn operand_names(self) -> &'static [&'static str] {
        NAMES[self as usize].1
    }
}

const _: () = {
    assert!(Opcode::COUNT <= 256);
    let mut index = 0;
    while index < Opcode::COUNT {
        assert!(DESCRIPTIONS[index].shapes.len() <= Opcode::MAX_OPERANDS);
        index += 1;
    }
};

/// An instruction with its operands as `Wide32` stores them, for code that goes by their shapes.
#[derive(Clone, Copy)]
pub struct RawInstruction {
    pub opcode: Opcode,
    /// In bytes, with the prefix.
    pub size: u8,
    words: [u32; Opcode::MAX_OPERANDS],
    pub metadata: u32,
}

impl RawInstruction {
    /// `None` unless a whole instruction starts at `offset`.
    pub fn read(code: &[u8], offset: u32) -> Option<RawInstruction> {
        let bytes = code.get(offset as usize..)?;
        match Opcode::from_byte(*bytes.first()?)? {
            Opcode::Wide16Prefix => Self::read_as::<Wide16>(bytes),
            Opcode::Wide32Prefix => Self::read_as::<Wide32>(bytes),
            _ => Self::read_as::<Narrow>(bytes),
        }
    }

    #[inline(always)]
    fn read_as<W: Width>(bytes: &[u8]) -> Option<RawInstruction> {
        let opcode = Opcode::from_byte(*bytes.get(W::PREFIX_BYTES)?)?;
        if matches!(opcode, Opcode::Wide16Prefix | Opcode::Wide32Prefix) {
            return None;
        }
        let size = opcode.size::<W>();
        let bytes = bytes.get(W::PREFIX_BYTES..size)?;
        let shapes = opcode.shapes();
        let mut words = [0; Opcode::MAX_OPERANDS];
        for (index, (word, &shape)) in words.iter_mut().zip(shapes).enumerate() {
            *word = if shape == Shape::Src {
                Operand::read::<W>(bytes, index).word()
            } else if shape.is_signed() {
                W::signed(bytes, index) as u32
            } else {
                W::unsigned(bytes, index)
            };
        }
        let metadata = match opcode.metadata() {
            Some(_) => W::metadata(bytes, shapes.len()),
            None => 0,
        };
        Some(RawInstruction {
            opcode,
            size: size as u8,
            words,
            metadata,
        })
    }

    #[inline]
    pub fn words(&self) -> &[u32] {
        &self.words[..self.opcode.shapes().len()]
    }
}

/// The instructions of a range of code with their offsets. Ends early at what is no instruction.
#[derive(Clone)]
pub struct Instructions<'a> {
    code: &'a [u8],
    offset: u32,
}

impl<'a> Instructions<'a> {
    #[inline]
    pub fn new(code: &'a [u8], range: Range<u32>) -> Instructions<'a> {
        Instructions {
            code: &code[..range.end as usize],
            offset: range.start,
        }
    }
}

impl Iterator for Instructions<'_> {
    type Item = (u32, RawInstruction);

    #[inline]
    fn next(&mut self) -> Option<(u32, RawInstruction)> {
        let offset = self.offset;
        let instruction = RawInstruction::read(self.code, offset)?;
        self.offset += u32::from(instruction.size);
        Some((offset, instruction))
    }
}

impl Instruction {
    /// The instruction at `offset` of verified code, and its size.
    pub fn read(code: &[u8], offset: u32) -> (Instruction, u32) {
        fn read_as<W: Width>(bytes: &[u8]) -> (Instruction, u32) {
            let bytes = &bytes[W::PREFIX_BYTES..];
            let opcode = Opcode::ALL[usize::from(bytes[0])];
            (
                Instruction::decode::<W>(opcode, bytes),
                opcode.size::<W>() as u32,
            )
        }
        let bytes = &code[offset as usize..];
        match Opcode::ALL[usize::from(bytes[0])] {
            Opcode::Wide16Prefix => read_as::<Wide16>(bytes),
            Opcode::Wide32Prefix => read_as::<Wide32>(bytes),
            _ => read_as::<Narrow>(bytes),
        }
    }
}
