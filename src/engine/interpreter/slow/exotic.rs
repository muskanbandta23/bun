use bun_engine_bytecode::Register;
use bun_engine_vm::code::metadata::{self, Flags, ForInGet, GetPrivateName, PutPrivateName};
use bun_engine_vm::ic::{self, Key};
use bun_engine_vm::{
    ClonedArguments, Cx, EnumerationMode, ForIn, JsObject, JsResult, JsSymbol, MappedArguments,
    Value, clone_object, copy_rest,
};

use super::properties::right_side_of_in;
use super::{Frame, Next, execute};

impl Frame<'_> {
    fn for_in(self, state: Register) -> ForIn {
        // SAFETY: what an instruction has put there.
        unsafe { ForIn::from_registers([0, 1, 2].map(|index| self.reg(state.nth(index)))) }
    }

    fn set_for_in(self, state: Register, for_in: ForIn) {
        for (index, part) in (0..).zip(for_in.to_registers()) {
            self.set(state.nth(index), part);
        }
    }

    /// Of an instruction that uses the current key on `base`.
    fn profiled_for_in(self, state: Register, entry: &metadata::ForIn, base: Value) -> ForIn {
        let for_in = self.for_in(state);
        record(&entry.modes, for_in.profile_for(base));
        if for_in.mode == EnumerationMode::Indexed {
            entry.array.record_base(base);
            if let Some(object) = base.as_object() {
                entry.array.record_index(&object, for_in.index, false);
            }
        }
        for_in
    }

    fn has_private(self, cx: &mut Cx, base: Value, name: Register, entry: u32) -> JsResult<bool> {
        let (object, symbol) = (right_side_of_in(cx, base)?, self.cell::<JsSymbol>(name));
        let (cases, name) = (self.entry(entry), symbol.name());
        ic::has_by_key(cx, self.code_block, cases, object, name, Key::Variable)
    }
}

fn record(modes: &Flags, seen: u32) {
    modes.0.set(modes.0.get() | seen);
}

execute! {
    CreateDirectArguments(op, cx, f) {
        f.set(op.dst, MappedArguments::direct_from_frame(cx, f.header)?);
    }
    CreateScopedArguments(op, cx, f) {
        f.set(op.dst, MappedArguments::scoped_from_frame(cx, f.header, f.cell(op.scope))?);
    }
    CreateUnmappedArguments(op, cx, f) {
        f.set(op.dst, ClonedArguments::from_frame(cx, f.header)?);
    }
    GetFromArguments(op, cx, f) {
        let arguments = f.cell::<MappedArguments>(op.arguments);
        // SAFETY: verified to be a parameter, each of which has a slot.
        f.set_profiled(op.dst, op.metadata, unsafe { arguments.slot_unchecked(op.index) }.get());
    }
    PutToArguments(op, cx, f) {
        let arguments = f.cell::<MappedArguments>(op.arguments);
        // SAFETY: as above.
        unsafe { arguments.slot_unchecked(op.index).set(&arguments, f.get(op.value)) };
    }
    EnsureArguments(op, cx, f) {
        if f.reg_or_empty(op.arguments).is_none() {
            if op.mapped {
                f.set(op.arguments, MappedArguments::direct_from_frame(cx, f.header)?);
            } else {
                f.set(op.arguments, ClonedArguments::from_frame(cx, f.header)?);
            }
        }
    }

    CopyDataProperties(op, cx, f) {
        f.cell::<JsObject>(op.target).copy_data_properties(cx, f.get(op.source))?;
    }
    CloneObject(op, cx, f) {
        f.set_profiled(op.dst, op.metadata, clone_object(cx, f.get(op.source))?);
    }
    CopyRest(op, cx, f) {
        let excluded = &f.code_block.code().name_lists[op.excluded.index()];
        let computed = f.collect(op.first, op.count);
        f.set(op.dst, copy_rest(cx, f.get(op.source), excluded, &computed)?);
    }

    GetPrivateName(op, cx, f) {
        let entry = f.entry::<GetPrivateName>(op.metadata);
        let value = entry.get_slow(cx, f.code_block, f.get(op.base), f.cell(op.name))?;
        f.set_profiled(op.dst, op.metadata, value);
    }
    PutPrivateName(op, cx, f) {
        let entry = f.entry::<PutPrivateName>(op.metadata);
        entry.set_slow(cx, f.code_block, f.get(op.base), f.cell(op.name), f.get(op.value))?;
    }
    DefinePrivateName(op, cx, f) {
        let entry = f.entry::<PutPrivateName>(op.metadata);
        entry.define_slow(cx, f.code_block, f.cell(op.base), f.cell(op.name), f.get(op.value))?;
    }
    HasPrivateName(op, cx, f) {
        f.set(op.dst, f.has_private(cx, f.get(op.base), op.name, op.metadata)?);
    }
    SetPrivateBrand(op, cx, f) {
        let entry = f.entry::<PutPrivateName>(op.metadata);
        entry.set_brand_slow(cx, f.code_block, f.cell(op.base), f.cell(op.brand))?;
    }
    CheckPrivateBrand(op, cx, f) {
        let entry = f.entry::<PutPrivateName>(op.metadata);
        entry.check_brand_slow(cx, f.code_block, f.get(op.base), f.cell(op.brand))?;
    }
    HasPrivateBrand(op, cx, f) {
        f.set(op.dst, f.has_private(cx, f.get(op.base), op.brand, op.metadata)?);
    }

    ForInOpen(op, cx, f) {
        let opened = ForIn::open(cx, f.get(op.base))?;
        match opened {
            Some(for_in) => f.set_for_in(op.state, for_in),
            None => (0..3).for_each(|index| f.set(op.state.nth(index), Value::UNDEFINED)),
        }
        Next::jump_if(opened.is_none())
    }
    ForInNext(op, cx, f) {
        let (mut for_in, base) = (f.for_in(op.state), f.reg(op.base));
        let entry = f.entry::<metadata::ForIn>(op.metadata);
        entry.array.record_base(base);
        let name = for_in.next(cx, base)?;
        if let Some(name) = name {
            record(&entry.modes, for_in.profile_for(base));
            f.set_for_in(op.state, for_in);
            f.set(op.name, name);
        }
        Next::jump_if(name.is_none())
    }
    ForInGet(op, cx, f) {
        let base = f.get(op.base);
        let for_in = f.profiled_for_in(op.state, &f.entry::<ForInGet>(op.metadata).for_in, base);
        f.set_profiled(op.dst, op.metadata, for_in.get(cx, base, f.cell(op.name))?);
    }
    ForInPut(op, cx, f) {
        let base = f.get(op.base);
        let for_in = f.profiled_for_in(op.state, f.entry(op.metadata), base);
        for_in.put(cx, base, f.cell(op.name), f.get(op.value), op.strict)?;
    }
    ForInHas(op, cx, f) {
        let base = f.get(op.base);
        let for_in = f.profiled_for_in(op.state, f.entry(op.metadata), base);
        f.set(op.dst, for_in.has(cx, base, f.cell(op.name))?);
    }
    ForInHasOwn(op, cx, f) {
        let base = f.get(op.base);
        let for_in = f.profiled_for_in(op.state, f.entry(op.metadata), base);
        f.set(op.dst, for_in.has_own(cx, base, f.cell(op.name))?);
    }
}
