//! Small, named fault injections prove the behavioral checks can fail.
//! Run explicitly with `cargo test -p native_contract_tests --test negative_controls
//! --release -- --ignored --nocapture`. No source in the working checkout is edited.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Mutation {
    name: &'static str,
    file: &'static str,
    before: &'static str,
    after: &'static str,
    package: &'static str,
    test: &'static str,
    assertion: &'static str,
}

const MUTATIONS: &[Mutation] = &[
    Mutation {
        name: "provider_io_holds_snapshot_mutex",
        file: "src/BetterTrafficMonitorAiUsage/UsageCoreAdapter.cpp",
        before: "    const bool refreshed = m_refresh(&snapshot) != 0;",
        after: "    bool refreshed{};\n    {\n        std::lock_guard<std::mutex> io_lock(m_mutex);\n        refreshed = m_refresh(&snapshot) != 0;\n    }",
        package: "native_contract_tests",
        test: "tests::getters_return_last_snapshot_while_provider_is_blocked",
        assertion: "snapshot getter waited for provider I/O",
    },
    Mutation {
        name: "parallel_refresh_gate_removed",
        file: "src/BetterTrafficMonitorAiUsage/UsageCoreAdapter.cpp",
        before: "    std::unique_lock<std::mutex> refresh_lock(m_refresh_mutex, std::try_to_lock);\n    if (!refresh_lock.owns_lock())\n        return;\n",
        after: "",
        package: "native_contract_tests",
        test: "tests::concurrent_refresh_returns_without_starting_another_request",
        assertion: "concurrent refresh waited behind provider I/O",
    },
    Mutation {
        name: "negative_zero_hour_loses_sign",
        file: "crates/usage_core/src/time_logic.rs",
        before: "    Some(if negative { -magnitude } else { magnitude })",
        after: "    Some(if negative && hours != 0 { -magnitude } else { magnitude })",
        package: "usage_core",
        test: "time_logic::tests::every_supported_utc_offset_round_trips_through_the_shipping_ffi",
        assertion: "assertion `left == right` failed",
    },
    Mutation {
        name: "overnight_week_wrap_removed",
        file: "crates/usage_core/src/time_logic.rs",
        before: "        if (now - start).rem_euclid(MINUTES_PER_WEEK) < length {",
        after: "        if now >= start && now < end {",
        package: "usage_core",
        test: "time_logic::tests::daily_overnight_window_matches_every_minute_including_week_wrap",
        assertion: "minute=0",
    },
];

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if matches!(entry.file_name().to_str(), Some("target" | ".git")) {
            continue;
        }
        let destination = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else if entry.file_type()?.is_file() {
            fs::copy(entry.path(), destination)?;
        } else {
            return Err(io::Error::other("source fixtures must be regular files"));
        }
    }
    Ok(())
}

fn cargo(source: &Path, build: &Path, package: &str, test: Option<&str>) -> io::Result<Output> {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(source)
        .env("CARGO_TARGET_DIR", build)
        .args([
            "test",
            "--release",
            "--locked",
            "--offline",
            "-p",
            package,
            "--lib",
        ]);
    if let Some(test) = test {
        command.args([test, "--", "--exact", "--nocapture", "--test-threads=1"]);
    } else {
        command.arg("--no-run");
    }
    command.output()
}

fn save_output(path: &Path, output: &Output) -> io::Result<String> {
    let text = format!(
        "exit_code={:?}\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    fs::write(path, &text)?;
    Ok(text)
}

#[test]
#[ignore = "explicit sensitivity check; builds isolated, intentionally broken variants"]
fn named_behavioral_mutations_are_detected() -> io::Result<()> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.parent().unwrap().parent().unwrap();
    // A distinct run directory isolates concurrent runs and preserves their logs.
    let run = root
        .join("target/negative-controls")
        .join(std::process::id().to_string());
    let source = run.join("source");
    let build = run.join("build");
    let evidence = run.join("evidence");
    fs::create_dir_all(&evidence)?;
    for file in ["Cargo.toml", "Cargo.lock"] {
        fs::create_dir_all(&source)?;
        fs::copy(root.join(file), source.join(file))?;
    }
    for directory in ["crates", "include", "src"] {
        copy_tree(&root.join(directory), &source.join(directory))?;
    }
    println!("Negative-control evidence: {}", evidence.display());

    for mutation in MUTATIONS {
        let path = source.join(mutation.file);
        let original = fs::read_to_string(&path)?;
        assert_eq!(
            original.matches(mutation.before).count(),
            1,
            "{}: production anchor changed",
            mutation.name
        );

        // First demonstrate that this exact named test passes unchanged.
        let baseline = cargo(&source, &build, mutation.package, Some(mutation.test))?;
        let text = save_output(
            &evidence.join(format!("{}-baseline.log", mutation.name)),
            &baseline,
        )?;
        assert!(
            baseline.status.success() && text.contains("1 passed; 0 failed"),
            "{}: baseline test did not pass; {}",
            mutation.name,
            text
        );

        fs::write(&path, original.replacen(mutation.before, mutation.after, 1))?;
        let compile = cargo(&source, &build, mutation.package, None)?;
        let compile_text = save_output(
            &evidence.join(format!("{}-compile.log", mutation.name)),
            &compile,
        )?;
        if !compile.status.success() {
            fs::write(&path, &original)?;
            panic!(
                "{}: a build failure is not a killed behavioral mutation; {}",
                mutation.name, compile_text
            );
        }
        let execution = cargo(&source, &build, mutation.package, Some(mutation.test))?;
        // Restore even a surviving mutation before checking its result. The real
        // checkout has never been modified, and only isolated copies are used.
        fs::write(&path, &original)?;
        let text = save_output(
            &evidence.join(format!("{}-test.log", mutation.name)),
            &execution,
        )?;
        assert!(
            !execution.status.success()
                && text.contains("test result: FAILED. 0 passed; 1 failed")
                && text.contains(mutation.test)
                && text.contains(mutation.assertion),
            "{}: expected named assertion did not detect the compiled fault; {}",
            mutation.name,
            text
        );
        println!(
            "DETECTED {}: compiled successfully; {} failed",
            mutation.name, mutation.test
        );
    }
    Ok(())
}
