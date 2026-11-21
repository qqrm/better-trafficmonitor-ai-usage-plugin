# Validation and mutation evidence

## Production binding

`native_contract_tests/build.rs` compiles the production `UsageCoreAdapter.cpp`.
Only its provider and monotonic clock are replaced with controlled inputs. The
tests execute the adapter's actual locking, single-flight refresh, cooldown,
publication and getter logic. Rust display-time tests also invoke the shipping
C ABI, with poisoned output fields and surrounding canaries.

The ABI contract compares 40 size/alignment/offset entries, including every
declared field in the five shared structures. Distinct values in every metric,
credit field, reset time and history point detect mapping mistakes that an
unchanged `sizeof` would miss. Snapshot copies and native getters are both checked.

Run the ordinary tests and the explicitly enabled regression controls:

```sh
cargo test --locked --release --workspace --all-targets
cargo test --release --locked -p native_contract_tests --test negative_controls -- --ignored --nocapture
```

The ordinary suite intentionally ignores the mutation runner. The quality workflow
explicitly invokes it on both Windows and Linux. For each of eight controls, the
runner requires a passing named baseline, a successful mutant compilation, and
the named test's expected assertion failure. Compilation failures, unrelated
failures and surviving mutants do not count. Only isolated source copies change.
CI uploads each baseline, compilation and execution log.

## Independent audit (2026-11-21)

Four additional faults were chosen independently of the original four controls.
The frozen source matched the published Git blobs at
`5215b0e6655b27889ccd5b27fd6a4000f8007897`. Each fault compiled and was tested
against the complete portable suite before any new assertions were added.

| Independent holdout | Original tests | Strengthened tests |
| --- | --- | --- |
| Return zero intensity inside a peak window | Passed | Fails the public FFI intensity assertions |
| Swap five-hour limit and seven-day usage fields in the C header | Passed | Fails both the full layout table and native value mapping |
| Treat provider return zero as a successful refresh | Fails | Still fails the last-successful-snapshot assertion |
| Truncate negative Unix seconds instead of flooring them | Passed | Fails the public FFI calendar result at Unix second -1 |

Thus the initial diagnostic sample contained one detected fault and three genuine
survivors. The four faults are now retained as regression controls alongside the
original four. Their subsequent detection is a regression result, not a fresh
independent mutation score. The final unmodified suite passes 43 Rust and seven
native adapter tests. These changes add assertions and fixtures; production logic
and the release version are unchanged.

## Automatic sweep of display-time logic

A separate unfiltered `cargo-mutants 27.1.0` sweep generated the same 82 candidates
before and after the test improvements. It mutates constants and the safe helpers
`digits`, `parse_minutes`, `format_utc_offset` and `peak_position_at`; it does not
automatically mutate the unsafe extern wrappers or the C++ adapter.

| Raw outcome | Original tests | Strengthened tests |
| --- | ---: | ---: |
| Detected by a failing test run | 66 | 80 |
| Survived | 15 | 1 |
| Did not compile | 1 | 1 |
| External timeout | 0 | 0 |

Of the final 80 detections, 77 are ordinary test failures and three abort the test
executable because injected division-by-zero faults panic across an `extern C`
boundary. These automatic outcomes are reported separately from the eight named
controls, which require an exact assertion failure. The generated day-length-zero
mutation does not compile and receives no credit as test detection.

The sole surviving mutation changes `nearest < 60` to `nearest <= 60`. Only the
case `nearest == 60` changes branches, and the new branch evaluates
`1.0 - 60.0 / 60.0 == 0.0`, exactly the original branch's result. It is equivalent
for the current implementation. No exclusion was added to conceal it: the raw
tool still reports one survivor and exits with code 2. This is a bounded tool
experiment, not proof that all faults in the project are detected.

The [machine-readable audit](validation/mutation-audit-2026-11-21.json) preserves
all 82 before/after classifications, the strengthened source hashes, failure
types, the equivalent-mutant argument and reproduction command. Reproduce with:

```sh
cargo install --locked cargo-mutants --version 27.1.0
cargo mutants --file crates/usage_core/src/time_logic.rs --test-workspace true --jobs 2 --jobserver-tasks 4 --timeout 30 --build-timeout 180 --cargo-arg=--locked
```

The audit additionally used `--cargo-arg=--offline` after dependencies were cached.
The bounded automatic sweep is an audit command; the existing CI gates execute
the ordinary tests and eight named regression controls.

## Limits

Controlled blocked-provider tests establish selected synchronization contracts;
they do not enumerate every interleaving or detect every C++ data race. The DLL
smoke driver exercises the packaged DLL, not the complete TrafficMonitor UI.
It does not poll external providers. `DataRequired` still polls synchronously on
the caller thread. Live provider correctness, extended host usage and device/UI
environment differences require separate evidence.

Mutation testing checks whether tests distinguish selected wrong programs.
Formal verification would instead require explicit properties and assumptions;
any claim would apply to those properties and the verified code/model only.
