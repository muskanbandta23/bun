//! A prototype that has a say in stores to indices takes the fast path away from the objects that inherit from it.

use crate::{
    FunctionRareData, GlobalObject, JsObject, RealmWatchpoint, TransitionKind, Vm, root_vec,
};

impl Vm {
    #[cold]
    pub(crate) fn did_add_indexed_interceptor_to_prototype(&self) {
        let _defer = self.heap.defer_collection();
        let (mut objects, mut globals, mut rare_data) = (root_vec(), root_vec(), root_vec());
        self.heap.for_each_live_cell(|cell| {
            if let Some(object) = cell.downcast::<JsObject>() {
                let mode = object.indexing_mode();
                if mode.kind().is_dense()
                    && !mode.is_slow_put()
                    && object.chain_may_intercept_indexed_access()
                {
                    objects.push(object);
                }
                globals.extend(object.downcast::<GlobalObject>());
            } else {
                rare_data.extend(cell.downcast::<FunctionRareData>());
            }
        });
        for global in globals {
            if !global.is_having_a_bad_time()
                && global.is_initialized()
                && global
                    .array_prototype()
                    .chain_may_intercept_indexed_access()
            {
                global.switch_array_shapes_to_slow_put(self);
                global
                    .watchpoints_unchecked(RealmWatchpoint::NotHavingABadTime)
                    .invalidate(self);
            }
        }
        self.shape_cache.clear();
        for rare_data in rare_data {
            if rare_data
                .subclass_shape()
                .is_some_and(|shape| shape.instance_indexing_mode().has_elements())
            {
                rare_data.clear(self);
            }
        }
        for object in objects {
            object.non_property_transition(self, TransitionKind::SwitchToSlowPut);
        }
    }
}
