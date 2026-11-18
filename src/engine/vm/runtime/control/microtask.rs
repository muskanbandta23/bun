use std::rc::Rc;

#[derive(Default)]
pub struct MicrotaskQueue {}

#[derive(Clone, Default)]
pub struct MicrotaskQueueRef(Rc<MicrotaskQueue>);

impl core::ops::Deref for MicrotaskQueueRef {
    type Target = MicrotaskQueue;

    fn deref(&self) -> &MicrotaskQueue {
        &self.0
    }
}
