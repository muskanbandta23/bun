use core::cell::Cell;

use bun_engine_platform::clock::monotonic_seconds;

/// Counts the seconds in which scripts were not paused.
pub(crate) struct Stopwatch {
    before_start: Cell<f64>,
    start: Cell<Option<f64>>,
}

impl Stopwatch {
    pub(crate) fn started() -> Stopwatch {
        Stopwatch {
            before_start: Cell::new(0.0),
            start: Cell::new(Some(monotonic_seconds())),
        }
    }

    pub(crate) fn elapsed(&self) -> f64 {
        let running = (self.start.get()).map_or(0.0, |start| monotonic_seconds() - start);
        self.before_start.get() + running
    }

    pub(crate) fn stop(&self) {
        self.before_start.set(self.elapsed());
        self.start.set(None);
    }

    pub(crate) fn resume(&self) {
        self.start.set(Some(monotonic_seconds()));
    }
}
