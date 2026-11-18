use bun_engine_bytecode::Register;
use bun_engine_vm::code::metadata::{Flags, IteratorNext, NewArrayWithSpecies};
use bun_engine_vm::{self as vm, ArrayAllocationProfile, IterationMode, IteratorRecord};

use super::{Frame, Next, execute};

impl Frame<'_> {
    fn iterator(self, state: Register) -> IteratorRecord {
        let parts = [2, 1, 0].map(|index| self.reg(state.nth(index)));
        // SAFETY: what an instruction has put there.
        unsafe { IteratorRecord::from_parts(parts) }
    }

    fn set_iterator(self, state: Register, record: IteratorRecord) {
        for (index, part) in [2, 1, 0].into_iter().zip(record.parts()) {
            self.set(state.nth(index), part);
        }
    }
}

fn record_mode(modes: &Flags, mode: IterationMode) {
    modes.0.set(modes.0.get() | mode.flag());
}

execute! {
    NewArray(op, cx, f) {
        let profile = f.entry::<ArrayAllocationProfile>(op.metadata);
        // SAFETY: the profile of a code block.
        f.set(op.dst, unsafe { vm::new_array(cx, profile, op.shape, f.run(op.first, op.count)) }?);
    }
    NewArrayFromConstant(op, cx, f) {
        let elements = f.code_block.constant(op.elements.0).expect("an array").cast();
        // SAFETY: as above.
        f.set(op.dst, unsafe { vm::new_array_from_constant(cx, f.entry(op.metadata), elements) }?);
    }
    NewArrayWithSize(op, cx, f) {
        // SAFETY: as above.
        f.set(op.dst, unsafe { vm::new_array_with_size(cx, f.entry(op.metadata), f.get(op.length)) }?);
    }
    NewArrayWithSpecies(op, cx, f) {
        let (entry, array) = (f.entry::<NewArrayWithSpecies>(op.metadata), f.get(op.array));
        entry.array.record_base(array);
        // SAFETY: as above.
        let result = unsafe { vm::new_array_with_species(cx, &entry.allocation, f.get(op.length), &array.cast::<vm::JsObject>()) }?;
        f.set_profiled(op.dst, op.metadata, result);
    }
    NewArrayWithSpread(op, cx, f) {
        f.set(op.dst, vm::new_array_with_spread(cx, f.run(op.first, op.count))?);
    }
    AppendToArray(op, cx, f) {
        vm::append_to_array(cx, &f.cell(op.array), f.run(op.first, op.count))?;
    }
    Spread(op, cx, f) {
        f.set(op.dst, vm::spread(cx, f.get(op.iterable))?);
    }
    ToArgumentList(op, cx, f) {
        let list = f.get_or_empty(op.value).map(|it| vm::to_argument_list(cx, it)).transpose()?;
        f.set_or_empty(op.dst, list);
    }
    IteratorOpen(op, cx, f) {
        let opened = IteratorRecord::open(cx, f.get(op.iterable), f.reg(op.method))?;
        record_mode(f.entry(op.metadata), opened.map_or(IterationMode::Generic, |it| it.mode()));
        match opened {
            Some(record) => f.set_iterator(op.state, record),
            None => f.set(op.state.nth(2), IteratorRecord::START),
        }
        Next::jump_if(opened.is_some())
    }
    IteratorNext(op, cx, f) {
        let mut record = f.iterator(op.state);
        record_mode(&f.entry::<IteratorNext>(op.metadata).modes, record.mode());
        if record.mode() == IterationMode::Generic {
            return Ok(Next::SecondTarget);
        }
        let value = record.step_builtin(cx)?;
        f.set_iterator(op.state, record);
        if let Some(value) = value {
            f.set_profiled(op.value, op.metadata, value);
        }
        Next::jump_if(value.is_none())
    }
    IteratorBegin(op, cx, f) {
        let record = IteratorRecord::begin(cx, f.get(op.iterable))?;
        record_mode(f.entry(op.metadata), record.mode());
        f.set_iterator(op.state, record);
    }
    IteratorStep(op, cx, f) {
        let mut record = f.iterator(op.state);
        record_mode(&f.entry::<IteratorNext>(op.metadata).modes, record.mode());
        let value = record.step_in_pattern(cx)?;
        f.set_iterator(op.state, record);
        f.set_profiled(op.value, op.metadata, value);
    }
    IteratorSkip(op, cx, f) {
        let mut record = f.iterator(op.state);
        record.skip_in_pattern(cx)?;
        f.set_iterator(op.state, record);
    }
    IteratorRest(op, cx, f) {
        let mut record = f.iterator(op.state);
        let rest = record.rest_in_pattern(cx)?;
        f.set_iterator(op.state, record);
        f.set(op.dst, rest);
    }
    IteratorClose(op, cx, f) {
        let record = f.iterator(op.state);
        record_mode(f.entry(op.metadata), record.mode());
        record.leave(cx, op.mode)?;
    }
}
