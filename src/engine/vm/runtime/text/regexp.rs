use bun_engine_macros::{Trace, cell};
use bun_engine_text::{StringRef, Text};

use crate::{Gc, JsCell, TypeFlags, Vm};

#[cell(name = "RegExp", cell_type = RegExp, flags = TypeFlags::SHAPE_IS_IMMORTAL)]
pub struct RegExp {
    base: JsCell,
    pattern: StringRef,
    flags: StringRef,
}

impl RegExp {
    pub fn for_literal(vm: &Vm, pattern: &StringRef, flags: Text<'_>) -> Gc<RegExp> {
        vm.allocate::<RegExp>(RegExpInit {
            base: vm.shapes.regexp(),
            pattern: pattern.clone(),
            flags: StringRef::from_text(flags),
        })
    }
}

#[derive(Default, Trace)]
pub struct RegExpGlobalState {}
