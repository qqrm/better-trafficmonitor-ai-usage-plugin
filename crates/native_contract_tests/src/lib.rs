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
            size_of::<UsageCoreHistoryPoint>(),
            size_of::<UsageCoreMetric>(),
            align_of::<UsageCoreMetric>(),
            offset_of!(UsageCoreMetric, history),
            size_of::<UsageCoreSnapshot>(),
            offset_of!(UsageCoreSnapshot, claude_next_reset_at_unix_seconds),
            offset_of!(UsageCoreSnapshot, zcode_5h),
            size_of::<UsageCorePeakSettings>(),
            size_of::<UsageCorePeakPosition>(),
            offset_of!(UsageCorePeakPosition, intensity),
        ];
        for (index, expected) in rust_values.into_iter().enumerate() {
            assert_eq!(
                unsafe { native_abi_value(index as u32) },
                expected,
                "ABI field {index}"
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
}
