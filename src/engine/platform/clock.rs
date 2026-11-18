/// From some moment before the process started. It never goes back.
#[inline]
pub fn monotonic_seconds() -> f64 {
    #[cfg(target_os = "macos")]
    return seconds_on(libc::CLOCK_UPTIME_RAW);
    #[cfg(all(unix, not(target_os = "macos")))]
    return seconds_on(libc::CLOCK_MONOTONIC);
    #[cfg(windows)]
    return performance_counter_seconds();
}

/// From the Unix epoch.
#[inline]
pub fn wall_seconds() -> f64 {
    #[cfg(unix)]
    return seconds_on(libc::CLOCK_REALTIME);
    #[cfg(windows)]
    return system_time_seconds();
}

#[cfg(unix)]
#[inline]
fn seconds_on(clock: libc::clockid_t) -> f64 {
    unsafe extern "C" {
        safe fn clock_gettime(clock: libc::clockid_t, time: &mut libc::timespec) -> libc::c_int;
    }
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    clock_gettime(clock, &mut time);
    time.tv_sec as f64 + time.tv_nsec as f64 / 1e9
}

#[cfg(windows)]
fn performance_counter_seconds() -> f64 {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        safe fn QueryPerformanceCounter(ticks: &mut i64) -> i32;
        safe fn QueryPerformanceFrequency(ticks_per_second: &mut i64) -> i32;
    }
    static SECONDS_PER_TICK: std::sync::LazyLock<f64> = std::sync::LazyLock::new(|| {
        let mut frequency = 1;
        QueryPerformanceFrequency(&mut frequency);
        1.0 / frequency as f64
    });
    let mut ticks = 0;
    QueryPerformanceCounter(&mut ticks);
    ticks as f64 * *SECONDS_PER_TICK
}

#[cfg(windows)]
fn system_time_seconds() -> f64 {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        safe fn GetSystemTimePreciseAsFileTime(ticks: &mut u64);
    }
    const TICKS_FROM_1601_TO_1970: u64 = 116_444_736_000_000_000;
    let mut ticks = 0;
    GetSystemTimePreciseAsFileTime(&mut ticks);
    ticks.wrapping_sub(TICKS_FROM_1601_TO_1970) as i64 as f64 / 1e7
}
