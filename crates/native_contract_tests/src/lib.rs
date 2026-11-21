//! Executable C++/Rust boundary checks; this crate is not linked into the plugin.

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::mem::{align_of, offset_of, size_of};
    use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};
    use usage_core::{
        UsageCoreHistoryPoint, UsageCoreMetric, UsageCorePeakPosition, UsageCorePeakSettings,
        UsageCoreSnapshot,
    };

    unsafe extern "C" {
        fn native_adapter_create(
            refresh: unsafe extern "C" fn(*mut UsageCoreSnapshot) -> i32,
            clock: extern "C" fn() -> u64,
        ) -> *mut c_void;
        fn native_adapter_destroy(adapter: *mut c_void);
        fn native_adapter_refresh(adapter: *mut c_void);
        fn native_adapter_percentage(adapter: *const c_void) -> f64;
        fn native_adapter_integer(adapter: *const c_void, field: u32) -> i64;
        fn native_adapter_first_history_percentage(adapter: *const c_void) -> f64;
        fn native_adapter_snapshot(adapter: *const c_void, out: *mut UsageCoreSnapshot);
        fn native_adapter_observe_metric(
            adapter: *const c_void,
            window: u32,
            integer_fields: *mut i64,
            floating_fields: *mut f64,
        );
        fn native_abi_count() -> usize;
        fn native_abi_value(index: u32) -> usize;
    }

    static SERIAL: Mutex<()> = Mutex::new(());
    static ACTIVE: Mutex<Option<Arc<Source>>> = Mutex::new(None);
    const CALL_DEADLINE: Duration = Duration::from_millis(250);

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    struct Source {
        state: Mutex<SourceState>,
        changed: Condvar,
    }

    struct SourceState {
        calls: usize,
        blocked: bool,
        successful: bool,
        percentage: f64,
        history_len: u32,
        clock_ms: u64,
    }

    impl Source {
        fn new() -> Self {
            Self {
                state: Mutex::new(SourceState {
                    calls: 0,
                    blocked: false,
                    successful: true,
                    percentage: 17.0,
                    history_len: 1,
                    clock_ms: 100,
                }),
                changed: Condvar::new(),
            }
        }

        fn wait_for_calls(&self, expected: usize) -> bool {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut state = lock(&self.state);
            while state.calls < expected {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    return false;
                };
                let (next, _) = self.changed.wait_timeout(state, remaining).unwrap();
                state = next;
            }
            true
        }

        fn release(&self) {
            lock(&self.state).blocked = false;
            self.changed.notify_all();
        }
    }

    fn active() -> Arc<Source> {
        lock(&ACTIVE).as_ref().expect("active test source").clone()
    }

    unsafe extern "C" fn refresh(out: *mut UsageCoreSnapshot) -> i32 {
        let source = active();
        let mut state = lock(&source.state);
        state.calls += 1;
        source.changed.notify_all();
        while state.blocked {
            state = source.changed.wait(state).unwrap();
        }
        if !state.successful {
            return 0;
        }
        let mut snapshot = UsageCoreSnapshot::default();
        snapshot.claude_5h.available = 1;
        snapshot.claude_5h.used_percentage = state.percentage;
        snapshot.claude_5h.reset_at_unix_seconds = 1_800_001_234;
        snapshot.claude_5h.history_len = state.history_len;
        snapshot.claude_5h.history[0] = UsageCoreHistoryPoint {
            timestamp_unix_seconds: 1_800_000_123,
            used_percentage: 12.5,
        };
        snapshot.claude_next_reset_at_unix_seconds = 1_800_002_345;
        snapshot.zcode_next_reset_at_unix_seconds = 1_800_003_456;
        snapshot.zcode_5h_used_units = 1234;
        snapshot.zcode_7d_limit_units = 60000;
        // SAFETY: the production adapter passes a live, writable stack snapshot.
        unsafe { out.write(snapshot) };
        1
    }

    extern "C" fn clock() -> u64 {
        lock(&active().state).clock_ms
    }

    struct Adapter(*mut c_void);

    // The production adapter explicitly synchronizes its shared snapshot and
    // serializes refreshes. These tests exercise that contract across threads.
    unsafe impl Send for Adapter {}
    unsafe impl Sync for Adapter {}

    impl Adapter {
        fn new() -> Self {
            Self(unsafe { native_adapter_create(refresh, clock) })
        }

        fn refresh(&self) {
            unsafe { native_adapter_refresh(self.0) }
        }

        fn percentage(&self) -> f64 {
            unsafe { native_adapter_percentage(self.0) }
        }

        fn snapshot(&self, out: &mut UsageCoreSnapshot) {
            unsafe { native_adapter_snapshot(self.0, out) }
        }
    }

    impl Drop for Adapter {
        fn drop(&mut self) {
            unsafe { native_adapter_destroy(self.0) }
        }
    }

    fn setup() -> (MutexGuard<'static, ()>, Arc<Source>, Arc<Adapter>) {
        let serial = lock(&SERIAL);
        let source = Arc::new(Source::new());
        *lock(&ACTIVE) = Some(source.clone());
        (serial, source, Arc::new(Adapter::new()))
    }

    #[test]
    fn getters_return_last_snapshot_while_provider_is_blocked() {
        let (_serial, source, adapter) = setup();
        adapter.refresh();
        {
            let mut state = lock(&source.state);
            state.blocked = true;
            state.percentage = 91.0;
            state.clock_ms += 10_000;
        }
        let writer = {
            let adapter = adapter.clone();
            thread::spawn(move || adapter.refresh())
        };
        let entered = source.wait_for_calls(2);
        let (tx, rx) = mpsc::channel();
        let reader = {
            let adapter = adapter.clone();
            thread::spawn(move || {
                let metric = adapter.percentage();
                let mut snapshot = UsageCoreSnapshot::default();
                adapter.snapshot(&mut snapshot);
                tx.send((metric, snapshot.claude_5h.used_percentage))
                    .unwrap();
            })
        };
        let result = rx.recv_timeout(CALL_DEADLINE);
        // Always unblock and join before asserting, including a regression.
        source.release();
        writer.join().unwrap();
        reader.join().unwrap();
        assert!(entered, "provider never entered the controlled wait");
        assert_eq!(
            result.expect("snapshot getter waited for provider I/O"),
            (17.0, 17.0)
        );
        assert_eq!(adapter.percentage(), 91.0);
    }

    #[test]
    fn concurrent_refresh_returns_without_starting_another_request() {
        let (_serial, source, adapter) = setup();
        lock(&source.state).blocked = true;
        let first = {
            let adapter = adapter.clone();
            thread::spawn(move || adapter.refresh())
        };
        let entered = source.wait_for_calls(1);
        let (tx, rx) = mpsc::channel();
        let second = {
            let adapter = adapter.clone();
            thread::spawn(move || {
                adapter.refresh();
                tx.send(()).unwrap();
            })
        };
        let returned = rx.recv_timeout(CALL_DEADLINE);
        source.release();
        first.join().unwrap();
        second.join().unwrap();
        assert!(entered, "provider never entered the controlled wait");
        returned.expect("concurrent refresh waited behind provider I/O");
        assert_eq!(lock(&source.state).calls, 1, "duplicate provider request");
    }

    #[test]
    fn failed_refresh_keeps_last_successful_snapshot_and_is_throttled() {
        let (_serial, source, adapter) = setup();
        adapter.refresh();
        {
            let mut state = lock(&source.state);
            state.clock_ms += 10_000;
            state.successful = false;
            state.percentage = 91.0;
        }
        adapter.refresh();
        adapter.refresh();
        assert_eq!(adapter.percentage(), 17.0);
        assert_eq!(lock(&source.state).calls, 2);
    }

    #[test]
    fn poll_interval_starts_at_completion_even_if_first_tick_is_zero() {
        let (_serial, source, adapter) = setup();
        {
            let mut state = lock(&source.state);
            state.clock_ms = 0;
            state.blocked = true;
        }
        let writer = {
            let adapter = adapter.clone();
            thread::spawn(move || adapter.refresh())
        };
        let entered = source.wait_for_calls(1);
        lock(&source.state).clock_ms = 30_000;
        source.release();
        writer.join().unwrap();
        assert!(entered);
        adapter.refresh();
        assert_eq!(
            lock(&source.state).calls,
            1,
            "slow poll immediately retried"
        );
        lock(&source.state).clock_ms += 9_999;
        adapter.refresh();
        assert_eq!(lock(&source.state).calls, 1);
        lock(&source.state).clock_ms += 1;
        adapter.refresh();
        assert_eq!(lock(&source.state).calls, 2);
    }

    #[test]
    fn native_and_rust_snapshot_layouts_match_and_copy_preserves_canaries() {
        let (_serial, _source, adapter) = setup();
        let rust_values = [
            (
                "UsageCoreHistoryPoint.size",
                size_of::<UsageCoreHistoryPoint>(),
            ),
            (
                "UsageCoreHistoryPoint.align",
                align_of::<UsageCoreHistoryPoint>(),
            ),
            (
                "UsageCoreHistoryPoint.timestamp_unix_seconds",
                offset_of!(UsageCoreHistoryPoint, timestamp_unix_seconds),
            ),
            (
                "UsageCoreHistoryPoint.used_percentage",
                offset_of!(UsageCoreHistoryPoint, used_percentage),
            ),
            ("UsageCoreMetric.size", size_of::<UsageCoreMetric>()),
            ("UsageCoreMetric.align", align_of::<UsageCoreMetric>()),
            (
                "UsageCoreMetric.available",
                offset_of!(UsageCoreMetric, available),
            ),
            (
                "UsageCoreMetric._padding",
                offset_of!(UsageCoreMetric, _padding),
            ),
            (
                "UsageCoreMetric.used_percentage",
                offset_of!(UsageCoreMetric, used_percentage),
            ),
            (
                "UsageCoreMetric.reset_at_unix_seconds",
                offset_of!(UsageCoreMetric, reset_at_unix_seconds),
            ),
            (
                "UsageCoreMetric.history_len",
                offset_of!(UsageCoreMetric, history_len),
            ),
            (
                "UsageCoreMetric._reserved",
                offset_of!(UsageCoreMetric, _reserved),
            ),
            (
                "UsageCoreMetric.history",
                offset_of!(UsageCoreMetric, history),
            ),
            ("UsageCoreSnapshot.size", size_of::<UsageCoreSnapshot>()),
            ("UsageCoreSnapshot.align", align_of::<UsageCoreSnapshot>()),
            (
                "UsageCoreSnapshot.claude_5h",
                offset_of!(UsageCoreSnapshot, claude_5h),
            ),
            (
                "UsageCoreSnapshot.claude_7d",
                offset_of!(UsageCoreSnapshot, claude_7d),
            ),
            (
                "UsageCoreSnapshot.codex_7d",
                offset_of!(UsageCoreSnapshot, codex_7d),
            ),
            (
                "UsageCoreSnapshot.claude_next_reset_at_unix_seconds",
                offset_of!(UsageCoreSnapshot, claude_next_reset_at_unix_seconds),
            ),
            (
                "UsageCoreSnapshot.zcode_5h",
                offset_of!(UsageCoreSnapshot, zcode_5h),
            ),
            (
                "UsageCoreSnapshot.zcode_7d",
                offset_of!(UsageCoreSnapshot, zcode_7d),
            ),
            (
                "UsageCoreSnapshot.zcode_next_reset_at_unix_seconds",
                offset_of!(UsageCoreSnapshot, zcode_next_reset_at_unix_seconds),
            ),
            (
                "UsageCoreSnapshot.zcode_5h_used_units",
                offset_of!(UsageCoreSnapshot, zcode_5h_used_units),
            ),
            (
                "UsageCoreSnapshot.zcode_5h_limit_units",
                offset_of!(UsageCoreSnapshot, zcode_5h_limit_units),
            ),
            (
                "UsageCoreSnapshot.zcode_7d_used_units",
                offset_of!(UsageCoreSnapshot, zcode_7d_used_units),
            ),
            (
                "UsageCoreSnapshot.zcode_7d_limit_units",
                offset_of!(UsageCoreSnapshot, zcode_7d_limit_units),
            ),
            (
                "UsageCorePeakSettings.size",
                size_of::<UsageCorePeakSettings>(),
            ),
            (
                "UsageCorePeakSettings.align",
                align_of::<UsageCorePeakSettings>(),
            ),
            (
                "UsageCorePeakSettings.enabled",
                offset_of!(UsageCorePeakSettings, enabled),
            ),
            (
                "UsageCorePeakSettings.start_minute",
                offset_of!(UsageCorePeakSettings, start_minute),
            ),
            (
                "UsageCorePeakSettings.end_minute",
                offset_of!(UsageCorePeakSettings, end_minute),
            ),
            (
                "UsageCorePeakSettings.utc_offset_minutes",
                offset_of!(UsageCorePeakSettings, utc_offset_minutes),
            ),
            (
                "UsageCorePeakSettings.weekdays_only",
                offset_of!(UsageCorePeakSettings, weekdays_only),
            ),
            (
                "UsageCorePeakPosition.size",
                size_of::<UsageCorePeakPosition>(),
            ),
            (
                "UsageCorePeakPosition.align",
                align_of::<UsageCorePeakPosition>(),
            ),
            (
                "UsageCorePeakPosition.inside",
                offset_of!(UsageCorePeakPosition, inside),
            ),
            (
                "UsageCorePeakPosition.minutes_until_start",
                offset_of!(UsageCorePeakPosition, minutes_until_start),
            ),
            (
                "UsageCorePeakPosition.minutes_until_end",
                offset_of!(UsageCorePeakPosition, minutes_until_end),
            ),
            (
                "UsageCorePeakPosition.reserved",
                offset_of!(UsageCorePeakPosition, reserved),
            ),
            (
                "UsageCorePeakPosition.intensity",
                offset_of!(UsageCorePeakPosition, intensity),
            ),
        ];
        assert_eq!(unsafe { native_abi_count() }, rust_values.len());
        for (index, (name, expected)) in rust_values.into_iter().enumerate() {
            assert_eq!(
                unsafe { native_abi_value(index as u32) },
                expected,
                "ABI field {name}"
            );
        }
        #[repr(C)]
        struct Guarded {
            before: u64,
            snapshot: UsageCoreSnapshot,
            after: u64,
        }
        let mut guarded = Guarded {
            before: 0x1234_5678_9abc_def0,
            snapshot: UsageCoreSnapshot::default(),
            after: 0xfedc_ba98_7654_3210,
        };
        adapter.refresh();
        for (field, expected) in [
            (0, 1_800_001_234),
            (1, 1_800_002_345),
            (2, 1_800_003_456),
            (3, 1234),
            (4, 60000),
            (5, 1),
            (6, 1_800_000_123),
        ] {
            assert_eq!(
                unsafe { native_adapter_integer(adapter.0, field) },
                expected
            );
        }
        assert_eq!(
            unsafe { native_adapter_first_history_percentage(adapter.0) },
            12.5
        );
        adapter.snapshot(&mut guarded.snapshot);
        assert_eq!(guarded.snapshot.claude_5h.used_percentage, 17.0);
        assert_eq!(guarded.before, 0x1234_5678_9abc_def0);
        assert_eq!(guarded.after, 0xfedc_ba98_7654_3210);
    }

    #[test]
    fn native_history_reader_bounds_a_malformed_ffi_length() {
        let (_serial, source, adapter) = setup();
        lock(&source.state).history_len = u32::MAX;
        adapter.refresh();
        assert_eq!(unsafe { native_adapter_integer(adapter.0, 5) }, 128);
        assert_eq!(
            unsafe { native_adapter_integer(adapter.0, 6) },
            1_800_000_123
        );
        assert_eq!(
            unsafe { native_adapter_first_history_percentage(adapter.0) },
            12.5
        );
    }

    fn distinct_metric(seed: u8, available: u8, history_len: u32) -> UsageCoreMetric {
        let mut metric = UsageCoreMetric {
            available,
            _padding: [seed; 7],
            used_percentage: f64::from(seed) + 0.25,
            reset_at_unix_seconds: 1_900_000_000 + i64::from(seed),
            history_len,
            _reserved: 0x1234_0000 + u32::from(seed),
            ..UsageCoreMetric::default()
        };
        // Every stored point, including unused capacity, differs between windows
        // and indices. A partial snapshot copy cannot accidentally match zeros.
        for (index, point) in metric.history.iter_mut().enumerate() {
            *point = UsageCoreHistoryPoint {
                timestamp_unix_seconds: 1_800_000_000 + i64::from(seed) * 1000 + index as i64,
                used_percentage: f64::from(seed) + index as f64 / 256.0,
            };
        }
        metric
    }

    fn distinct_snapshot() -> UsageCoreSnapshot {
        UsageCoreSnapshot {
            claude_5h: distinct_metric(11, 1, 1),
            claude_7d: distinct_metric(22, 0, 2),
            codex_7d: distinct_metric(33, 2, 3),
            claude_next_reset_at_unix_seconds: 1_910_000_001,
            zcode_5h: distinct_metric(44, 3, 4),
            zcode_7d: distinct_metric(55, 255, 5),
            zcode_next_reset_at_unix_seconds: 1_920_000_002,
            zcode_5h_used_units: 5432,
            zcode_5h_limit_units: 12345,
            zcode_7d_used_units: 23456,
            zcode_7d_limit_units: 67890,
        }
    }

    unsafe extern "C" fn refresh_distinct_snapshot(out: *mut UsageCoreSnapshot) -> i32 {
        // SAFETY: the production adapter supplies a writable snapshot.
        unsafe { out.write(distinct_snapshot()) };
        1
    }

    extern "C" fn fixed_clock() -> u64 {
        100
    }

    #[test]
    fn all_snapshot_fields_and_native_window_mappings_preserve_distinct_values() {
        let adapter =
            Adapter(unsafe { native_adapter_create(refresh_distinct_snapshot, fixed_clock) });
        adapter.refresh();
        let expected = distinct_snapshot();
        #[repr(C)]
        struct Guarded {
            before: u64,
            snapshot: UsageCoreSnapshot,
            after: u64,
        }
        let mut copied = Guarded {
            before: 0x1234_5678_9abc_def0,
            snapshot: UsageCoreSnapshot::default(),
            after: 0xfedc_ba98_7654_3210,
        };
        adapter.snapshot(&mut copied.snapshot);
        assert_eq!(copied.before, 0x1234_5678_9abc_def0);
        assert_eq!(copied.after, 0xfedc_ba98_7654_3210);
        let actual = &copied.snapshot;
        for (window, (name, actual, expected)) in [
            ("Claude5h", &actual.claude_5h, &expected.claude_5h),
            ("Claude7d", &actual.claude_7d, &expected.claude_7d),
            ("Codex7d", &actual.codex_7d, &expected.codex_7d),
            ("ZCode5h", &actual.zcode_5h, &expected.zcode_5h),
            ("ZCode7d", &actual.zcode_7d, &expected.zcode_7d),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(actual.available, expected.available, "{name} availability");
            assert_eq!(
                actual._padding, expected._padding,
                "{name} explicit padding"
            );
            assert_eq!(
                actual.used_percentage, expected.used_percentage,
                "{name} percentage"
            );
            assert_eq!(
                actual.reset_at_unix_seconds, expected.reset_at_unix_seconds,
                "{name} reset"
            );
            assert_eq!(
                actual.history_len, expected.history_len,
                "{name} history length"
            );
            assert_eq!(actual._reserved, expected._reserved, "{name} reserved");
            for (index, (actual, expected)) in
                actual.history.iter().zip(&expected.history).enumerate()
            {
                assert_eq!(
                    (actual.timestamp_unix_seconds, actual.used_percentage),
                    (expected.timestamp_unix_seconds, expected.used_percentage),
                    "{name} stored history point {index}"
                );
            }

            let mut integers = [i64::MIN; 5];
            let mut floats = [f64::NAN; 3];
            unsafe {
                native_adapter_observe_metric(
                    adapter.0,
                    window as u32,
                    integers.as_mut_ptr(),
                    floats.as_mut_ptr(),
                )
            };
            let first = &expected.history[0];
            let last = &expected.history[expected.history_len as usize - 1];
            assert_eq!(
                integers,
                [
                    i64::from(expected.available != 0),
                    expected.reset_at_unix_seconds,
                    i64::from(expected.history_len),
                    first.timestamp_unix_seconds,
                    last.timestamp_unix_seconds,
                ],
                "{name} native integer fields"
            );
            assert_eq!(
                floats,
                [
                    expected.used_percentage,
                    first.used_percentage,
                    last.used_percentage
                ],
                "{name} native floating fields"
            );
        }
        assert_eq!(actual.claude_next_reset_at_unix_seconds, 1_910_000_001);
        assert_eq!(actual.zcode_next_reset_at_unix_seconds, 1_920_000_002);
        assert_eq!(actual.zcode_5h_used_units, 5432);
        assert_eq!(actual.zcode_5h_limit_units, 12345);
        assert_eq!(actual.zcode_7d_used_units, 23456);
        assert_eq!(actual.zcode_7d_limit_units, 67890);
        for (field, expected) in [
            (1, 1_910_000_001),
            (2, 1_920_000_002),
            (3, 5432),
            (4, 67890),
            (7, 12345),
            (8, 23456),
        ] {
            assert_eq!(
                unsafe { native_adapter_integer(adapter.0, field) },
                expected,
                "native reset/credit field {field}"
            );
        }
    }
}
