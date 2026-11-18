use bun_engine_bytecode::Specialization;

use super::{CallTarget, stubs};
use crate::{CellType, Cx, Gc, JsCell, JsFunction, JsResult, Value};

/// For a loop that calls the same function on each turn. It is used from about as deep in the stack as it was made.
#[derive(Clone, Copy)]
pub struct RepeatedCall {
    callee: Value,
    /// The same, if nothing has to be looked at before it is entered.
    function: Option<Gc<JsCell>>,
}

impl RepeatedCall {
    #[inline]
    pub fn new(cx: &Cx, callee: Value) -> JsResult<RepeatedCall> {
        cx.check_stack()?;
        let function = (callee.as_cell())
            .filter(|cell| CellType::FUNCTIONS.contains(cell.cell_type()) && cx.is_entered());
        Ok(RepeatedCall { callee, function })
    }

    /// Call(F, V, argumentsList)
    #[inline(always)]
    pub fn call<const COUNT: usize>(
        &self,
        cx: &mut Cx,
        this: Value,
        arguments: [Value; COUNT],
    ) -> JsResult<Value> {
        const { assert!(COUNT <= 3) };
        let Some(function) = self.function else {
            return cx.call(self.callee, this, &arguments);
        };
        debug_assert!(!cx.has_exception() && cx.is_entered());
        let argument = |index: usize| arguments.get(index).copied().unwrap_or_default();
        // SAFETY: both kinds of function have an executable, and the entry goes with it.
        unsafe {
            let executable = JsFunction::executable_or_rare_data(function);
            let target = CallTarget::of(&executable, Specialization::Call);
            stubs::enter_with_registers(
                target.entry,
                cx,
                Value::NUMBER_TAG,
                Value::NOT_CELL_MASK,
                target.code_block,
                self.callee,
                this,
                target.executable,
                COUNT as u64 + 1,
                argument(0),
                argument(1),
                argument(2),
            )
        }
    }
}
