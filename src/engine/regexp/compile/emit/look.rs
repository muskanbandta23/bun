//! Lookahead and lookbehind.

use super::Emitter;
use super::state::Fail;
use crate::Error;
use crate::compile::graph::{Id, MAX_REACH, Traits};
use crate::program::{Direction, Op};

impl Emitter<'_> {
    pub(super) fn look(
        &mut self,
        body: Id,
        direction: Direction,
        negated: bool,
        fail: Fail,
    ) -> Result<Fail, Error> {
        let outer = core::mem::replace(&mut self.direction, direction);
        let (repeats, following) = (self.repeats, core::mem::take(&mut self.following));
        match negated {
            true => self.negative_look(body, fail)?,
            false => self.positive_look(body, fail)?,
        }
        (self.direction, self.repeats, self.following) = (outer, repeats, following);
        Ok(fail)
    }

    /// Whether going back to before the node has to count as work.
    pub(super) fn is_costly(&self, id: Id) -> bool {
        self.repeats && self.graph[id].max > MAX_REACH
    }

    fn positive_look(&mut self, body: Id, fail: Fail) -> Result<(), Error> {
        let (entry, offset) = (self.entry(), self.state.offset);
        let origin = match self.graph[body].is(Traits::RIGID) {
            true => None,
            false => Some(self.anchor(self.state.room)?),
        };
        self.need(self.graph[body].min, fail)?;
        self.node(body, 0, fail)?;
        if let Some(from) = origin
            && self.state.is_reachable
            && !self.is_unchanged_since(entry)
        {
            self.commit()?;
            self.weigh(from.slot);
            let metered = self.is_costly(body);
            self.op(Op::Load { from, metered })?;
            self.state.room = self.room_at(from).unwrap_or_default();
            self.state.anchor = Some(from);
        }
        self.state.offset = offset;
        Ok(())
    }

    fn negative_look(&mut self, body: Id, fail: Fail) -> Result<(), Error> {
        let passed = self.output.label()?;
        let not_found = match self.graph[body].is(Traits::RIGID) {
            true => self.fail_directly_to(passed)?,
            false => {
                let undo = self.plan_undo(self.is_costly(body))?;
                self.fail_to(passed, undo)?
            }
        };
        let before = self.snapshot()?;
        self.need(self.graph[body].min, not_found)?;
        self.node(body, 0, not_found)?;
        self.write_tests()?;
        self.jump_to_fail(fail)?;
        self.restore(&before)?;
        self.bind(passed)
    }
}
