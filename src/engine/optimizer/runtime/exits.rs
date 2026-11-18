//! Leaving optimized code: reading the values out and building the frames of the tier below.

use core::any::Any;
use core::cell::{Cell, RefCell};

use bun_engine_bytecode::Register;
use bun_engine_macros::native_abi;
use bun_engine_vm::{CallFrame, CodeBlock, Cx, Gc, RootVec, Vm, call, root_vec};
use smallvec::SmallVec;

use super::code::Compiled;
use super::machine::{Saved, Transfer};
use super::values::{self, StateFrame};
use crate::graph::{ARGUMENT_COUNT, FrameIndex, FrameInfo};
use crate::lower::output::{EXIT_REGISTER, ExitRecord};

/// How often code may leave for the tier below before it is thrown away. Each attempt lasts longer than the last.
fn allowance(reoptimizations: u32) -> u32 {
    100 << reoptimizations.min(5)
}

/// Where an exit builds the frames before they are put on the stack. One per thread.
pub(crate) struct Scratch {
    /// From the lowest address up. The collector scans it, so it holds the values of the frames safely.
    image: RefCell<RootVec<u64>>,
    transfer: Cell<Transfer>,
}

impl Default for Scratch {
    fn default() -> Scratch {
        Scratch {
            image: RefCell::new(root_vec()),
            transfer: Cell::new(Transfer::default()),
        }
    }
}

/// Builds the frames of the tier below where the optimized frame is, and says where to carry on.
///
/// # Safety
/// The thunk has just saved the machine state of a frame of optimized code at `frame_pointer`.
#[native_abi]
pub(super) unsafe fn leave(vm: &Vm, saved: &Saved, frame_pointer: usize) -> *const Transfer {
    // SAFETY: the caller's.
    let frame = unsafe { CallFrame::from_address(frame_pointer) };
    let code_block = frame.code_block().expect("optimized code has one");
    let compiled = code_block.compiled().expect("the frame is running it");
    let any: &dyn Any = &*compiled;
    let compiled: &Compiled = any.downcast_ref().expect("this optimizer made it");

    let record = compiled.output.exits[saved.gpr(EXIT_REGISTER.number()) as usize];
    let values =
        &compiled.output.exit_values[record.first_value as usize..][..record.value_count as usize];
    let states = values::frames(&compiled.output, record.layout, values);
    debug_assert_eq!(states[0].frame, FrameIndex::ROOT);

    let mut exit = Exit {
        vm,
        compiled,
        saved,
        frame,
        writes: SmallVec::new(),
        pointers: SmallVec::new(),
        counts: SmallVec::new(),
    };
    exit.read(&states);
    let scratch = vm.extension::<Scratch>();
    let transfer = exit.write(&states, &mut scratch.image.borrow_mut());
    scratch.transfer.set(transfer);
    count(vm, code_block, compiled, &record);
    scratch.transfer.as_ptr()
}

/// A speculation has failed, so the next compile does not make it, and the code goes once it has failed too often.
fn count(vm: &Vm, code_block: Gc<CodeBlock>, compiled: &Compiled, record: &ExitRecord) {
    if !record.kind.is_counted() {
        return;
    }
    compiled
        .code_block_of(record.frame)
        .add_exit_site(record.bytecode_offset, record.kind);
    let taken = compiled.exits_taken().get() + 1;
    compiled.exits_taken().set(taken);
    if taken >= allowance(code_block.baseline().reoptimizations()) {
        code_block.baseline().count_reoptimization();
        code_block.jettison(vm);
    }
}

/// What goes into one slot of a frame that is being built.
struct Write {
    address: usize,
    bits: u64,
}

struct Exit<'a> {
    vm: &'a Vm,
    compiled: &'a Compiled,
    saved: &'a Saved,
    frame: &'a CallFrame,
    writes: SmallVec<[Write; 24]>,
    /// Where the frame pointer of each frame of the state goes.
    pointers: SmallVec<[usize; 6]>,
    /// Including `this`, for each frame that was inlined.
    counts: SmallVec<[u32; 6]>,
}

/// The words of a frame from its pointer up: the header, `this` and the arguments.
fn top_words(state: &StateFrame<'_>) -> u32 {
    let highest = state.registers.iter().map(|register| register.offset() + 1);
    highest
        .max()
        .unwrap_or(0)
        .max(Register::HEADER_WORDS as i32 + 1) as u32
}

/// Where the frame pointer of each frame of a state goes. The first is the frame that is on the stack; each of the
/// others lies below the locals of its caller, in an even number of words.
fn pointers(frames: &[FrameInfo], top: usize, states: &[StateFrame<'_>]) -> SmallVec<[usize; 6]> {
    let mut pointer = top;
    let mut pointers = SmallVec::new();
    for (position, state) in states.iter().enumerate() {
        if position != 0 {
            let caller = frames[states[position - 1].frame.index()];
            let words = caller.frame_words + 1 + top_words(state);
            pointer -= words.next_multiple_of(2) as usize * 8;
        }
        pointers.push(pointer);
    }
    pointers
}

impl Exit<'_> {
    /// What a place has, in the form that the slot of the lower tier wants.
    fn value_of(&self, state: &StateFrame<'_>, position: usize) -> u64 {
        let value = state.values[position];
        // SAFETY: the places are of the frame whose registers the thunk saved.
        let bits = unsafe { values::read(value.place, self.frame.address(), Some(self.saved)) };
        values::tagged(bits, value.format)
    }

    /// Reads every value out before anything is written over, and works out where the frames go.
    fn read(&mut self, states: &[StateFrame<'_>]) {
        let frames = &self.compiled.output.frames;
        self.pointers = pointers(frames, self.frame.address(), states);
        self.counts = SmallVec::from_elem(0, states.len());
        for (frame, state) in states.iter().enumerate() {
            let pointer = self.pointers[frame];
            for (position, &register) in state.registers.iter().enumerate() {
                let bits = self.value_of(state, position);
                if register == ARGUMENT_COUNT {
                    self.counts[frame] = bits as u32;
                } else {
                    self.writes.push(Write {
                        address: pointer.wrapping_add_signed(register.offset() as isize * 8),
                        bits,
                    });
                }
            }
        }
    }

    /// The lowest address that the frames reach, which is where the interpreter leaves its stack pointer.
    fn lowest(&self, frames: usize, states: &[StateFrame<'_>]) -> usize {
        let innermost = self.compiled.frame_at(states[frames - 1].frame);
        self.pointers[frames - 1] - innermost.frame_words as usize * 8 - 8
    }

    /// What the frame's call-site slot holds while the interpreter is at that instruction.
    fn resume_at(&self, state: &StateFrame<'_>) -> u32 {
        self.compiled
            .code_block_of(state.frame)
            .call_site_of(state.resume)
    }

    /// Where the interpreter goes on in the caller once the call returns.
    fn return_address(&self, state: &StateFrame<'_>) -> usize {
        let code_block = self.compiled.code_block_of(state.frame);
        let instructions = code_block.body().instructions();
        bun_engine_interpreter::return_point(instructions, state.resume)
            .expect("a call was inlined there")
    }

    /// The header of a frame that the interpreter did not build. The callee comes from the state.
    fn header(&self, frame: usize, states: &[StateFrame<'_>]) -> [(usize, u64); 4] {
        let state = &states[frame];
        let code_block = self.compiled.code_block_of(state.frame);
        let caller = self.pointers[frame - 1];
        let linkage = u64::from(self.resume_at(state)) << 32 | u64::from(self.counts[frame]);
        [
            (CallFrame::OFFSET_OF_CALLER, caller as u64),
            (
                CallFrame::OFFSET_OF_RETURN_ADDRESS,
                self.return_address(&states[frame - 1]) as u64,
            ),
            (CallFrame::OFFSET_OF_CODE_BLOCK, code_block.address() as u64),
            (CallFrame::OFFSET_OF_ARGUMENT_COUNT, linkage),
        ]
    }

    /// Fills the image and the frame that is already on the stack, and says where to carry on.
    fn write(&mut self, states: &[StateFrame<'_>], image: &mut RootVec<u64>) -> Transfer {
        let overflows = self.lowest(states.len(), states) < self.vm.soft_stack_limit();
        let frames = if overflows { 1 } else { states.len() };
        let (lowest, highest) = (self.lowest(frames, states), self.frame.address());
        image.clear();
        image.resize((highest - lowest) / 8, 0);
        let mut put = |address: usize, bits: u64| {
            if (lowest..highest).contains(&address) {
                image[(address - lowest) / 8] = bits;
            } else if address >= highest {
                // SAFETY: a slot of the frame on the stack, which the image does not cover.
                unsafe { *core::ptr::with_exposed_provenance_mut::<u64>(address) = bits }
            }
        };
        for frame in 1..frames {
            for (offset, bits) in self.header(frame, states) {
                put(self.pointers[frame] + offset, bits);
            }
        }
        for write in &self.writes {
            put(write.address, write.bits);
        }

        let root = self.compiled.code_block_of(FrameIndex::ROOT).baseline();
        // SAFETY: the interpreter runs the baseline code block of the function that has the frame.
        unsafe { self.frame.set_code_block(Some(root)) };
        self.frame.set_call_site(self.resume_at(&states[0]));
        Transfer {
            source: image.as_ptr().expose_provenance(),
            destination: lowest,
            words: image.len(),
            frame_pointer: self.pointers[frames - 1],
            target: if overflows {
                self.throw_stack_overflow();
                call::throw_from_frame()
            } else {
                bun_engine_interpreter::resume_entry()
            },
        }
    }

    /// There is no room for the frames that the calls which were inlined would have had.
    #[cold]
    fn throw_stack_overflow(&self) {
        let global = self.compiled.code_block_of(FrameIndex::ROOT).global();
        self.vm.set_top_call_frame(self.frame);
        // SAFETY: inside the call that entered the code, which runs in that realm.
        let cx = unsafe { Cx::from_global(global) };
        let _ = cx.throw_stack_overflow();
    }
}
