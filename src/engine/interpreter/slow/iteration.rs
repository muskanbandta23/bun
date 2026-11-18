use super::{execute, not_supported};

not_supported! {
    NewArrayWithSize NewArrayWithSpecies NewArrayWithSpread AppendToArray Spread ToArgumentList
    IteratorOpen IteratorNext IteratorBegin IteratorStep IteratorSkip IteratorRest IteratorClose
}

execute! {
    NewArray(op, cx, f) {
        let array = cx.new_array_with_length(bun_engine_vm::ElementKind::Undecided, op.count)?;
        for (index, value) in (0..).zip(f.run(op.first, op.count).iter().rev()) {
            if let &Some(value) = value {
                array.put_direct_index(cx, index, value)?;
            }
        }
        f.set(op.dst, array);
    }
    NewArrayFromConstant(op, cx, f) {
        let elements = f.code_block.constant(op.elements.0).expect("an array").cast();
        f.set(op.dst, bun_engine_vm::JsArray::from_immutable(cx, elements)?);
    }
}
