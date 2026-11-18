//! What compiled code, the stubs and the run time agree on: registers, frames and the layout of data.

use core::mem::offset_of;
use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicUsize};

use bun_engine_asm::{Fpr, Gpr, Reg, abi};
use bun_engine_wasm_format::{
    FuncIndex, FuncType, GlobalIndex, MemoryIndex, ModuleHeader, TableIndex, TagIndex, TypeIndex,
    ValKind, ValType,
};
use smallvec::SmallVec;

pub const INSTANCE: Gpr = abi::VM;
/// Of memory 0. With [`MemoryMode::Checked`] it means nothing across calls.
pub const MEMORY_BASE: Gpr = abi::NUMBER_TAG;
/// At the entry of a function that is called through a [`FunctionSlot`] or an [`Import`], the address of that.
pub const SLOT: Gpr = abi::NON_ARGUMENT_GPRS[0];

pub const ARGUMENT_GPRS: &[Gpr] = &abi::ARGUMENT_GPRS;
pub const ARGUMENT_FPRS: &[Fpr] = &abi::ARGUMENT_FPRS;
pub const RESULT_GPRS: &[Gpr] = &abi::RETURN_GPRS;
pub const RESULT_FPRS: &[Fpr] = abi::ARGUMENT_FPRS.split_at(2).0;

/// Offsets from the frame pointer. The caller makes room for the two words above the return address, and the
/// function fills them in.
pub mod frame {
    pub const INSTANCE: i32 = 16;
    pub const CALLEE: i32 = 24;
    pub const ARGUMENTS: i32 = 32;
    /// Where the frame pointer of the callee will be, from the stack pointer of the caller.
    pub const CALLEE_FRAME_FROM_STACK_POINTER: i32 = -16;
}

/// The low bits of the word in the place of the callee, which no cell has.
pub const CALLEE_TAG: u32 = 3;

/// Fits in 31 bits.
pub const fn callee_word(func: FuncIndex) -> u32 {
    func.0 << 2 | CALLEE_TAG
}

/// References are JavaScript values. An `i31ref` is an int32 whose sign is extended from 31 bits.
pub const NULL: i32 = 0x02;
pub const INT32_TAG: u64 = 0xfffe_0000_0000_0000;

pub const PAGE_SIZE_LOG2: u8 = 16;
/// What follows the 4 GiB of a guarded memory. An access with `offset + size` up to this needs no check.
pub const GUARD_SIZE: u64 = 1 << 31;
/// A function that calls nothing and has a frame up to this size does not check the stack.
pub const UNCHECKED_FRAME_SIZE: u32 = 256;

/// How memory 0 is accessed. Other memories are always checked.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MemoryMode {
    /// It does not move, and a fault tells of an access that is out of bounds.
    Guarded,
    Checked,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    Register(Reg),
    /// From the frame pointer of the callee.
    Stack(i32),
}

/// An `i32` in a register has the upper half clear. On the stack it takes 8 bytes of which the low 4 count.
pub struct Signature {
    pub params: SmallVec<[Place; 8]>,
    pub results: SmallVec<[Place; 2]>,
    /// From [`frame::ARGUMENTS`] up, a multiple of 16. Results end at the top of it, and a tail call keeps that top
    /// where it is.
    pub stack_bytes: u32,
}

fn assign<A: smallvec::Array<Item = Place>>(
    types: &[ValType],
    gprs: &[Gpr],
    fprs: &[Fpr],
) -> (SmallVec<A>, u32) {
    let (mut gprs, mut fprs, mut bytes) = (gprs.iter(), fprs.iter(), 0u32);
    let places = types.iter().map(|ty| {
        let register = match ty.kind() {
            ValKind::F32 | ValKind::F64 | ValKind::V128 => fprs.next().map(|&reg| Reg::from(reg)),
            _ => gprs.next().map(|&reg| Reg::from(reg)),
        };
        register.map_or_else(
            || {
                let size = ty.size_in_bytes().max(8);
                let start = bytes.next_multiple_of(size);
                bytes = start + size;
                Place::Stack(frame::ARGUMENTS + start as i32)
            },
            Place::Register,
        )
    });
    (places.collect(), bytes.next_multiple_of(16))
}

impl Signature {
    pub fn new(ty: FuncType<'_>) -> Self {
        let (params, param_bytes) = assign(ty.params, ARGUMENT_GPRS, ARGUMENT_FPRS);
        let (mut results, result_bytes): (SmallVec<_>, _) =
            assign(ty.results, RESULT_GPRS, RESULT_FPRS);
        let stack_bytes = param_bytes.max(result_bytes);
        for place in &mut results {
            if let Place::Stack(offset) = place {
                *offset += (stack_bytes - result_bytes) as i32;
            }
        }
        Self {
            params,
            results,
            stack_bytes,
        }
    }
}

/// One for each function that a module defines and each [`MemoryMode`], shared by instances and threads.
#[repr(C)]
pub struct FunctionSlot {
    /// The best code so far, or the stub that compiles some.
    pub entry: AtomicUsize,
    /// Baseline code counts up, without synchronization, and asks for better code when this is no longer negative.
    pub countdown: AtomicI32,
}

impl FunctionSlot {
    pub const SIZE: usize = size_of::<Self>();
    pub const ENTRY: i32 = offset_of!(Self, entry) as i32;
    pub const COUNTDOWN: i32 = offset_of!(Self, countdown) as i32;
}

/// What baseline code has seen at a call site.
#[repr(C)]
pub struct CallProfile {
    /// Of the first function that an indirect call reached, or zero.
    pub slot: AtomicUsize,
    pub count: AtomicU32,
    /// How many of the calls went elsewhere.
    pub misses: AtomicU32,
}

impl CallProfile {
    pub const SIZE: usize = size_of::<Self>();
    pub const SLOT: i32 = offset_of!(Self, slot) as i32;
    pub const COUNT: i32 = offset_of!(Self, count) as i32;
    pub const MISSES: i32 = offset_of!(Self, misses) as i32;
}

/// How to call a function: with `instance` in [`INSTANCE`], its memory in [`MEMORY_BASE`] and `slot` in [`SLOT`],
/// at the address that `slot` points to.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Callable {
    pub slot: usize,
    pub instance: usize,
    /// Zero in an element of a table that is null.
    pub rtt: usize,
}

impl Callable {
    pub const SLOT: i32 = offset_of!(Self, slot) as i32;
    pub const INSTANCE: i32 = offset_of!(Self, instance) as i32;
    pub const RTT: i32 = offset_of!(Self, rtt) as i32;
}

/// An element of a table of functions. Other tables have references of 8 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FunctionElement {
    pub callable: Callable,
    pub reference: u64,
}

impl FunctionElement {
    pub const SIZE_LOG2: u8 = size_of::<Self>().ilog2() as u8;
    pub const REFERENCE: i32 = offset_of!(Self, reference) as i32;
}

const _: () = assert!(size_of::<FunctionElement>().is_power_of_two());

/// For a function of the host, the slot of `callable` is `stub`, and the instance is the one that imports.
#[repr(C)]
pub struct Import {
    pub callable: Callable,
    pub stub: usize,
    pub host_function: u64,
    pub host_data: usize,
}

impl Import {
    pub const STUB: i32 = offset_of!(Self, stub) as i32;
    pub const HOST_FUNCTION: i32 = offset_of!(Self, host_function) as i32;
    pub const HOST_DATA: i32 = offset_of!(Self, host_data) as i32;
}

/// The instance's view of a memory, brought up to date when the memory grows.
#[repr(C)]
pub struct MemoryView {
    pub base: usize,
    /// In bytes. Not for shared memories.
    pub length: u64,
    /// For shared memories, the address of an [`AtomicU64`] with the length in bytes.
    pub shared_length: usize,
}

impl MemoryView {
    pub const BASE: i32 = offset_of!(Self, base) as i32;
    pub const LENGTH: i32 = offset_of!(Self, length) as i32;
    pub const SHARED_LENGTH: i32 = offset_of!(Self, shared_length) as i32;
}

const _: () = assert!(size_of::<AtomicU64>() == 8);

/// The start of a table, to which instances point.
#[repr(C)]
pub struct TableHeader {
    pub elements: usize,
    pub length: u64,
}

impl TableHeader {
    pub const ELEMENTS: i32 = offset_of!(Self, elements) as i32;
    pub const LENGTH: i32 = offset_of!(Self, length) as i32;
}

/// Where things are in the objects of the heap.
#[derive(Clone, Copy, Debug)]
pub struct HostLayout {
    /// The bytes at the start of an instance that are the host's, a multiple of 16.
    pub instance_header_size: u32,
    /// A byte of every cell.
    pub cell_kind: i32,
    pub struct_kind: u8,
    /// One more than `struct_kind`.
    pub array_kind: u8,
    /// A byte of every cell. After a reference is stored in the cell, [`Operation::WriteBarrier`] is due if the byte
    /// is not above `barrier_threshold`.
    pub cell_state: i32,
    pub barrier_threshold: u8,
    /// The address of the `Rtt` of a struct or an array.
    pub object_rtt: i32,
    pub struct_fields: i32,
    /// 32 bits.
    pub array_length: i32,
    pub array_elements: i32,
    /// The [`Callable`] in the object that a `funcref` is.
    pub function_callable: i32,
    /// An access to null plus an offset below this faults. Zero if faults are not handled.
    pub null_guard_size: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlobalPlace {
    /// The value is at this offset in the instance.
    Inline(i32),
    /// The address of the value is. References are written through [`Operation::GlobalSetReference`].
    Indirect(i32),
}

/// Offsets in an instance, which is a cell.
pub struct InstanceLayout {
    header: i32,
    memories: i32,
    tables: i32,
    imports: i32,
    tags: i32,
    types: i32,
    function_references: i32,
    globals: Vec<GlobalPlace>,
    size: u32,
}

impl InstanceLayout {
    const FIXED_WORDS: i32 = 4;

    /// The limits on what a module declares keep the size below 2^27.
    pub fn new(module: &ModuleHeader, host: &HostLayout) -> Self {
        let header = host.instance_header_size as i32;
        let mut end = header + Self::FIXED_WORDS * 8;
        let mut take = |count: usize, size: usize| {
            let start = end;
            end += (count * size) as i32;
            start
        };
        // An instance without memory has a view too, for those who load the base without asking.
        let memories = take(module.memories().len().max(1), size_of::<MemoryView>());
        let tables = take(module.tables().len(), 8);
        let imported_globals = module.imported().globals as usize;
        let place = |(index, global): (usize, &bun_engine_wasm_format::Global)| {
            if global.mutable && (global.exported || index < imported_globals) {
                GlobalPlace::Indirect(take(1, 8))
            } else {
                GlobalPlace::Inline(take(1, global.ty.size_in_bytes().max(8) as usize))
            }
        };
        let globals = module.globals().iter().enumerate().map(place).collect();
        let imports = take(module.imported().funcs as usize, size_of::<Import>());
        let tags = take(module.tags().len(), 8);
        let types = take(module.types().len() as usize, 8);
        let function_references = take(module.funcs().len(), 8);
        Self {
            header,
            memories,
            tables,
            imports,
            tags,
            types,
            function_references,
            globals,
            size: end as u32,
        }
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn vm(host: &HostLayout) -> i32 {
        host.instance_header_size as i32
    }

    /// The stack pointer must stay above the address here.
    pub fn stack_limit(host: &HostLayout) -> i32 {
        Self::vm(host) + 8
    }

    /// The address of what the run time keeps for the instance.
    pub fn runtime_data(host: &HostLayout) -> i32 {
        Self::vm(host) + 16
    }

    /// The same for every instance.
    pub fn memory_base(host: &HostLayout) -> i32 {
        Self::vm(host) + Self::FIXED_WORDS * 8 + MemoryView::BASE
    }

    pub fn header_size(&self) -> i32 {
        self.header
    }

    pub fn memory(&self, memory: MemoryIndex) -> i32 {
        self.memories + memory.0 as i32 * size_of::<MemoryView>() as i32
    }

    /// The address of a [`TableHeader`].
    pub fn table(&self, table: TableIndex) -> i32 {
        self.tables + table.0 as i32 * 8
    }

    pub fn import(&self, func: FuncIndex) -> i32 {
        self.imports + func.0 as i32 * size_of::<Import>() as i32
    }

    /// A word that identifies the tag.
    pub fn tag(&self, tag: TagIndex) -> i32 {
        self.tags + tag.0 as i32 * 8
    }

    /// A word of the host's for making objects of the type.
    pub fn type_data(&self, ty: TypeIndex) -> i32 {
        self.types + ty.0 as i32 * 8
    }

    /// The `funcref`, or zero until [`Operation::RefFunc`] makes it.
    pub fn function_reference(&self, func: FuncIndex) -> i32 {
        self.function_references + func.0 as i32 * 8
    }

    pub fn global(&self, global: GlobalIndex) -> GlobalPlace {
        self.globals[global.usize()]
    }
}

macro_rules! numbered {
    ($(#[$meta:meta])* $name:ident { $($(#[$variant_meta:meta])* $variant:ident,)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        #[repr(u8)]
        pub enum $name {
            $($(#[$variant_meta])* $variant,)*
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];
            pub const COUNT: usize = Self::ALL.len();
        }
    };
}

numbered! {
    Trap {
        Unreachable,
        MemoryOutOfBounds,
        UnalignedAtomic,
        TableOutOfBounds,
        IndirectCallOutOfBounds,
        IndirectCallToNull,
        SignatureMismatch,
        DivisionByZero,
        IntegerOverflow,
        TruncationOutOfRange,
        StackOverflow,
        NullReference,
        NullExnref,
        NullI31,
        NullStruct,
        NullArray,
        NullFunction,
        NullAsNonNull,
        CastFailure,
        ArrayOutOfBounds,
        OutOfMemory,
    }
}

numbered! {
    /// Functions of the run time. They follow the C calling convention and take the instance first, then the
    /// immediates, then the operands, addresses and lengths as 64 bits. One that fails unwinds and does not return.
    Operation {
        MemoryGrow,
        MemoryFill,
        MemoryCopy,
        MemoryInit,
        DataDrop,
        AtomicWait32,
        AtomicWait64,
        AtomicNotify,
        TableSet,
        TableGrow,
        TableFill,
        TableCopy,
        TableInit,
        ElemDrop,
        RefFunc,
        GlobalSetReference,
        /// Takes the cell.
        WriteBarrier,
        /// Gives a struct whose fields have their defaults.
        StructNew,
        /// Gives an array whose elements have their default.
        ArrayNew,
        ArrayNewData,
        ArrayNewElem,
        ArrayFill,
        ArrayCopy,
        ArrayInitData,
        ArrayInitElem,
        AnyConvertExtern,
        /// Takes the tag and the address of the payload, laid out like the fields of a struct.
        Throw,
        ThrowRef,
        /// For a handler. Gives the `exnref`.
        TakeException,
        /// Takes an `exnref`. Gives the word that identifies its tag, or zero.
        ExceptionTag,
        /// Takes an `exnref`. Gives the address of the payload.
        ExceptionPayload,
    }
}

/// Addresses of code that is the same for all modules.
pub struct Stubs {
    pub operations: [usize; Operation::COUNT],
    /// Called, so that the return address tells where. The frame is entered and filled in.
    pub traps: [usize; Trap::COUNT],
    /// Called from the prologue of baseline code. Keeps all registers.
    pub tier_up_at_entry: usize,
    /// Called from the start of a loop with its index in [`SLOT`], when all values are in the frame. Returns in
    /// [`SLOT`] the address at which optimized code takes over the frame, or zero.
    pub tier_up_in_loop: usize,
}
