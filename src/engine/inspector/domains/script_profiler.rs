use core::cell::Cell;

use bun_engine_vm::{Cx, Vm};

use crate::Controller;
use crate::protocol::{Object, Outcome, Request, object};

/// Who has called into scripts.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProfilingReason {
    Api,
    Microtask,
    Other,
}

#[derive(Default)]
pub(crate) struct ScriptProfiler {
    is_tracking: Cell<bool>,
    is_evaluating: Cell<bool>,
}

impl ScriptProfiler {
    /// Stops without a result.
    pub(crate) fn abandon(&self, _: &Vm) {
        self.is_tracking.set(false);
        self.is_evaluating.set(false);
    }
}

impl Controller {
    pub(crate) fn start_tracking_scripts(&self, _: &Vm, _includes_samples: bool) {
        if self.script_profiler.is_tracking.replace(true) {
            return;
        }
        let params = object! { "timestamp": self.stopwatch.elapsed() };
        self.notify("ScriptProfiler.trackingStart", params);
    }

    pub(crate) fn stop_tracking_scripts(&self, vm: &Vm) {
        if !self.script_profiler.is_tracking.get() {
            return;
        }
        let params = object! { "timestamp": self.stopwatch.elapsed() };
        self.script_profiler.abandon(vm);
        self.notify("ScriptProfiler.trackingComplete", params);
    }

    /// What to pass to `did_evaluate_script`. `None` if that is not to be called.
    pub fn will_evaluate_script(&self) -> Option<f64> {
        let profiler = &self.script_profiler;
        (profiler.is_tracking.get() && !profiler.is_evaluating.replace(true))
            .then(|| self.stopwatch.elapsed())
    }

    pub fn did_evaluate_script(&self, start: f64, reason: ProfilingReason) {
        if !self.script_profiler.is_evaluating.take() {
            return;
        }
        let event = object! {
            "startTime": start,
            "endTime": self.stopwatch.elapsed(),
            "type": match reason {
                ProfilingReason::Api => "API",
                ProfilingReason::Microtask => "Microtask",
                ProfilingReason::Other => "Other",
            },
        };
        self.notify("ScriptProfiler.trackingUpdate", object! { "event": event });
    }

    pub(crate) fn dispatch_script_profiler(
        &self,
        cx: &mut Cx,
        method: &str,
        request: &Request<'_>,
    ) -> Outcome<Option<Object>> {
        let params = &request.params;
        match method {
            "startTracking" => {
                let includes_samples = params.flag("includeSamples");
                params.check()?;
                self.start_tracking_scripts(cx, includes_samples);
            }
            "stopTracking" => self.stop_tracking_scripts(cx),
            _ => return Err(params.no_such_method()),
        }
        Ok(Some(object! {}))
    }
}
