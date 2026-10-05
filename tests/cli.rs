use std::fs;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use tempfile::TempDir;

fn bin() -> Command {
    Command::cargo_bin("slake").expect("binary should build")
}

fn temp_paths() -> (TempDir, String, String) {
    let temp = TempDir::new().expect("tempdir");
    let db = temp.path().join("runs.sqlite3");
    let marker = temp.path().join("marker.txt");

    (
        temp,
        db.to_string_lossy().into_owned(),
        marker.to_string_lossy().into_owned(),
    )
}

#[test]
fn run_executes_command_on_first_invocation() {
    let (_temp, db, marker) = temp_paths();

    let mut command = bin();
    command.args([
        "--db",
        &db,
        "run",
        "--name",
        "backup",
        "--min-interval",
        "30m",
        "--",
        "sh",
        "-c",
        &format!("printf first >> {marker}"),
    ]);
    command.assert().success().stdout(contains("action=run"));

    assert_eq!(fs::read_to_string(marker).unwrap(), "first");
}

#[test]
fn run_skips_when_cooldown_window_is_active() {
    let (_temp, db, marker) = temp_paths();

    let mut first = bin();
    first.args([
        "--db",
        &db,
        "run",
        "--name",
        "backup",
        "--min-interval",
        "30m",
        "--",
        "sh",
        "-c",
        &format!("printf first >> {marker}"),
    ]);
    first.assert().success();

    let mut second = bin();
    second.args([
        "--db",
        &db,
        "run",
        "--name",
        "backup",
        "--min-interval",
        "30m",
        "--",
        "sh",
        "-c",
        &format!("printf second >> {marker}"),
    ]);
    second.assert().success().stdout(contains("action=skip"));

    assert_eq!(fs::read_to_string(marker).unwrap(), "first");
}

#[test]
fn clear_resets_saved_state() {
    let (_temp, db, marker) = temp_paths();

    let mut run = bin();
    run.args([
        "--db",
        &db,
        "run",
        "--name",
        "backup",
        "--min-interval",
        "30m",
        "--",
        "sh",
        "-c",
        &format!("printf first >> {marker}"),
    ]);
    run.assert().success();

    let mut clear = bin();
    clear.args(["--db", &db, "clear", "--name", "backup"]);
    clear
        .assert()
        .success()
        .stdout(contains("action=clear").and(contains("deleted_runs=1")));

    let mut status = bin();
    status.args([
        "--db",
        &db,
        "status",
        "--name",
        "backup",
        "--min-interval",
        "30m",
    ]);
    status
        .assert()
        .success()
        .stdout(contains("state=never-run"));
}

#[test]
fn clear_refuses_live_claim_without_force() {
    let (_temp, db, marker) = temp_paths();
    let started = format!("{marker}.started");
    let mut running = std::process::Command::new(env!("CARGO_BIN_EXE_slake"))
        .args([
            "--db",
            &db,
            "run",
            "--name",
            "backup",
            "--min-interval",
            "10m",
            "--lease",
            "2s",
            "--",
            "sh",
            "-c",
            &format!("printf started > {started}; sleep 0.5; printf A >> {marker}"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("long command should start");
    let wait_started = Instant::now();
    while !std::path::Path::new(&started).exists() {
        assert!(wait_started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(10));
    }

    let mut guarded_clear = bin();
    guarded_clear.args(["--db", &db, "clear", "--name", "backup"]);
    guarded_clear
        .assert()
        .failure()
        .stderr(contains("active claim"));

    let mut status = bin();
    status.args([
        "--db",
        &db,
        "status",
        "--name",
        "backup",
        "--min-interval",
        "10m",
    ]);
    status
        .assert()
        .success()
        .stdout(contains("state=cooling-down"));

    let mut force_clear = bin();
    force_clear.args(["--db", &db, "clear", "--name", "backup", "--force"]);
    force_clear
        .assert()
        .success()
        .stdout(contains("action=clear"));
    assert!(!running.wait().expect("child finishes").success());
}

#[test]
fn expired_lease_allows_overlap_and_stale_owner_cannot_finalize() {
    let (_temp, db, marker) = temp_paths();
    let started = format!("{marker}.started");
    let mut first = std::process::Command::new(env!("CARGO_BIN_EXE_slake"))
        .args([
            "--db",
            &db,
            "run",
            "--name",
            "backup",
            "--min-interval",
            "10m",
            "--lease",
            "100ms",
            "--",
            "sh",
            "-c",
            &format!("printf started > {started}; sleep 0.5; printf A >> {marker}"),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("first command should start");
    let wait_started = Instant::now();
    while !std::path::Path::new(&started).exists() {
        assert!(wait_started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(180));
    assert!(
        first.try_wait().unwrap().is_none(),
        "first child should still be running"
    );

    let mut second = bin();
    second.args([
        "--db",
        &db,
        "run",
        "--name",
        "backup",
        "--min-interval",
        "10m",
        "--",
        "sh",
        "-c",
        &format!("printf B >> {marker}"),
    ]);
    second.assert().success().stdout(contains("action=run"));
    assert!(!first.wait().expect("first command finishes").success());
    let text = fs::read_to_string(marker).unwrap();
    assert!(text.contains('A') && text.contains('B'));
}

#[test]
fn concurrent_runs_respect_cooldown_when_invoked_together() {
    let (_temp, db, marker) = temp_paths();

    let first_db = db.clone();
    let first_marker = marker.clone();
    let first = thread::spawn(move || {
        let mut command = bin();
        command.args([
            "--db",
            &first_db,
            "run",
            "--name",
            "backup",
            "--min-interval",
            "10m",
            "--",
            "sh",
            "-c",
            &format!("sleep 0.3; printf A >> {first_marker}"),
        ]);
        let output = command.output().expect("first command should execute");
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).into_owned()
    });

    thread::sleep(Duration::from_millis(25));

    let second_db = db.clone();
    let second_marker = marker.clone();
    let second = thread::spawn(move || {
        let mut command = bin();
        command.args([
            "--db",
            &second_db,
            "run",
            "--name",
            "backup",
            "--min-interval",
            "10m",
            "--",
            "sh",
            "-c",
            &format!("printf B >> {second_marker}"),
        ]);
        let output = command.output().expect("second command should execute");
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).into_owned()
    });

    let first_output = first.join().unwrap();
    let second_output = second.join().unwrap();

    assert!(first_output.contains("action=run"));
    assert!(second_output.contains("action=skip"));

    assert_eq!(fs::read_to_string(marker).unwrap(), "A");
}

#[test]
fn unrelated_jobs_are_not_blocked_by_a_running_command() {
    let (_temp, db, marker) = temp_paths();
    let started_marker = format!("{marker}.started");
    let first_db = db.clone();
    let first_marker = marker.clone();
    let first_started_marker = started_marker.clone();
    let first = thread::spawn(move || {
        let mut command = bin();
        command.args([
            "--db",
            &first_db,
            "run",
            "--name",
            "long-job",
            "--min-interval",
            "10m",
            "--",
            "sh",
            "-c",
            &format!(
                "printf started > {first_started_marker}; sleep 0.6; printf A >> {first_marker}"
            ),
        ]);
        command.output().expect("long command should execute")
    });

    let wait_started = Instant::now();
    while !std::path::Path::new(&started_marker).exists() {
        assert!(wait_started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(10));
    }

    let started = Instant::now();
    let mut second = bin();
    second.args([
        "--db",
        &db,
        "run",
        "--name",
        "short-job",
        "--min-interval",
        "10m",
        "--",
        "sh",
        "-c",
        &format!("printf B >> {marker}"),
    ]);
    second.assert().success().stdout(contains("action=run"));
    assert!(started.elapsed() < Duration::from_millis(400));

    assert!(first.join().unwrap().status.success());
    let contents = fs::read_to_string(marker).unwrap();
    assert!(contents.contains('A'));
    assert!(contents.contains('B'));
}

#[test]
fn spawn_failure_is_recorded_and_uses_configured_backoff() {
    let (_temp, db, _marker) = temp_paths();
    let missing = "/definitely/not/a/slake-command";

    let mut first = bin();
    first.args([
        "--db",
        &db,
        "run",
        "--name",
        "missing-command",
        "--min-interval",
        "1ms",
        "--failure-backoff",
        "10m",
        "--",
        missing,
    ]);
    first.assert().code(2).stderr(contains("failed to execute"));

    let mut second = bin();
    second.args([
        "--db",
        &db,
        "run",
        "--name",
        "missing-command",
        "--min-interval",
        "1ms",
        "--failure-backoff",
        "10m",
        "--",
        missing,
    ]);
    second.assert().success().stdout(contains("action=skip"));
}

#[test]
fn nonzero_execution_uses_configured_failure_backoff() {
    let (_temp, db, marker) = temp_paths();

    let mut first = bin();
    first.args([
        "--db",
        &db,
        "run",
        "--name",
        "failing-command",
        "--min-interval",
        "1ms",
        "--failure-backoff",
        "10m",
        "--",
        "sh",
        "-c",
        &format!("printf A >> {marker}; exit 7"),
    ]);
    first.assert().code(7).stdout(contains("action=run"));

    let mut second = bin();
    second.args([
        "--db",
        &db,
        "run",
        "--name",
        "failing-command",
        "--min-interval",
        "1ms",
        "--failure-backoff",
        "10m",
        "--",
        "sh",
        "-c",
        &format!("printf B >> {marker}; exit 7"),
    ]);
    second.assert().success().stdout(contains("action=skip"));
    assert_eq!(fs::read_to_string(marker).unwrap(), "A");
}

#[test]
fn invalid_job_names_are_rejected() {
    let (_temp, db, _marker) = temp_paths();
    let mut command = bin();
    command.args([
        "--db",
        &db,
        "status",
        "--name",
        "not normalized",
        "--min-interval",
        "1s",
    ]);
    command
        .assert()
        .code(2)
        .stderr(contains("job name must start"));
}

fn alias_bin() -> Command {
    Command::cargo_bin("cooldown-guard").expect("alias binary should build")
}

#[test]
fn deprecated_alias_prints_one_note_and_behaves_like_slake() {
    let (_temp, db, marker) = temp_paths();

    let mut alias = alias_bin();
    alias.args([
        "--db",
        &db,
        "run",
        "--name",
        "backup",
        "--min-interval",
        "30m",
        "--",
        "sh",
        "-c",
        &format!("printf alias >> '{marker}'"),
    ]);
    let output = alias.assert().success().get_output().clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("deprecated"), "{stderr}");
    assert!(stderr.contains("slake"), "{stderr}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("name=backup action=run exit_code=0"));
    assert!(!stdout.contains("deprecated"));
    assert_eq!(fs::read_to_string(&marker).unwrap(), "alias");

    // The ledger the alias wrote is the one slake reads.
    let mut status = bin();
    status.args([
        "--db",
        &db,
        "status",
        "--name",
        "backup",
        "--min-interval",
        "30m",
    ]);
    status
        .assert()
        .success()
        .stdout(contains("state=cooling-down"));

    // Same stdout and exit code as slake for a deterministic query.
    let args = [
        "--db",
        &db,
        "--json",
        "status",
        "--name",
        "never",
        "--min-interval",
        "1s",
    ];
    let slake_out = bin().args(args).assert().success().get_output().clone();
    let alias_out = alias_bin()
        .args(args)
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(slake_out.stdout, alias_out.stdout);
    assert!(slake_out.stderr.is_empty());

    // Usage errors keep exit code 2 under the alias too.
    alias_bin()
        .args(["--db", &db, "status", "--name", "x"])
        .assert()
        .code(2)
        .stderr(contains("deprecated"));
}

/// Default ledger locations for a sandboxed HOME / XDG layout.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod default_ledger {
    use std::path::{Path, PathBuf};

    use assert_cmd::Command;
    use predicates::str::contains;
    use tempfile::TempDir;

    pub struct Sandbox {
        pub temp: TempDir,
    }

    impl Sandbox {
        pub fn new() -> Self {
            Self {
                temp: TempDir::new().expect("tempdir"),
            }
        }

        fn root(&self) -> &Path {
            self.temp.path()
        }

        pub fn command(&self, bin: &str) -> Command {
            let mut command = Command::cargo_bin(bin).expect("binary should build");
            command
                .env("HOME", self.root().join("home"))
                .env("XDG_STATE_HOME", self.root().join("state"))
                .env("XDG_DATA_HOME", self.root().join("data"))
                .env_remove("XDG_CONFIG_HOME")
                .env_remove("XDG_CACHE_HOME");
            command
        }

        #[cfg(target_os = "linux")]
        pub fn slake_ledger(&self) -> PathBuf {
            self.root().join("state/slake/runs.sqlite3")
        }

        #[cfg(target_os = "linux")]
        pub fn legacy_ledgers(&self) -> Vec<PathBuf> {
            vec![
                self.root().join("state/cooldown-guard/runs.sqlite3"),
                self.root().join("data/cooldown-guard/runs.sqlite3"),
            ]
        }

        #[cfg(target_os = "macos")]
        pub fn slake_ledger(&self) -> PathBuf {
            self.root()
                .join("home/Library/Application Support/tech.Greyforge.slake/runs.sqlite3")
        }

        #[cfg(target_os = "macos")]
        pub fn legacy_ledgers(&self) -> Vec<PathBuf> {
            vec![self.root().join(
                "home/Library/Application Support/tech.Greyforge.cooldown-guard/runs.sqlite3",
            )]
        }

        /// Records a successful `name` run in the ledger at `path`.
        pub fn seed(&self, path: &Path, name: &str) {
            self.command("slake")
                .arg("--db")
                .arg(path)
                .args(["run", "--name", name, "--min-interval", "1h", "--", "true"])
                .assert()
                .success()
                .stdout(contains("action=run"));
        }

        pub fn default_status(&self, bin: &str, name: &str) -> String {
            let output = self
                .command(bin)
                .args(["status", "--name", name, "--min-interval", "1h"])
                .assert()
                .success()
                .get_output()
                .clone();
            String::from_utf8(output.stdout).unwrap()
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn default_ledger_uses_existing_cooldown_guard_ledger_in_place() {
    let candidates = default_ledger::Sandbox::new().legacy_ledgers().len();
    for index in 0..candidates {
        let sandbox = default_ledger::Sandbox::new();
        let legacy = sandbox.legacy_ledgers()[index].clone();
        sandbox.seed(&legacy, "backup");
        let before = fs::read(&legacy).unwrap();

        for bin in ["slake", "cooldown-guard"] {
            assert!(
                sandbox
                    .default_status(bin, "backup")
                    .contains("state=cooling-down"),
                "{bin} should read the legacy ledger at {}",
                legacy.display()
            );
        }
        assert!(
            !sandbox.slake_ledger().exists(),
            "the legacy ledger must not be copied to the slake location"
        );

        // A guarded run through the default path keeps writing the legacy ledger.
        sandbox
            .command("slake")
            .args([
                "run",
                "--name",
                "other",
                "--min-interval",
                "1h",
                "--",
                "true",
            ])
            .assert()
            .success();
        assert!(!sandbox.slake_ledger().exists());
        assert_ne!(fs::read(&legacy).unwrap(), before);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn default_ledger_prefers_slake_ledger_when_both_exist() {
    let sandbox = default_ledger::Sandbox::new();
    for legacy in sandbox.legacy_ledgers() {
        sandbox.seed(&legacy, "legacy-job");
    }
    sandbox.seed(&sandbox.slake_ledger(), "slake-job");

    for bin in ["slake", "cooldown-guard"] {
        assert!(
            sandbox
                .default_status(bin, "slake-job")
                .contains("state=cooling-down")
        );
        assert!(
            sandbox
                .default_status(bin, "legacy-job")
                .contains("state=never-run")
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn default_ledger_is_created_at_slake_location_when_none_exists() {
    let sandbox = default_ledger::Sandbox::new();
    sandbox
        .command("slake")
        .args([
            "run",
            "--name",
            "fresh",
            "--min-interval",
            "1h",
            "--",
            "true",
        ])
        .assert()
        .success();
    assert!(sandbox.slake_ledger().is_file());
    for legacy in sandbox.legacy_ledgers() {
        assert!(!legacy.exists());
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn explicit_db_ignores_default_and_legacy_ledgers() {
    let sandbox = default_ledger::Sandbox::new();
    let legacy = sandbox.legacy_ledgers()[0].clone();
    sandbox.seed(&legacy, "backup");
    let explicit = sandbox.temp.path().join("explicit/ledger.sqlite3");

    sandbox
        .command("slake")
        .arg("--db")
        .arg(&explicit)
        .args(["status", "--name", "backup", "--min-interval", "1h"])
        .assert()
        .success()
        .stdout(contains("state=never-run"));
    assert!(explicit.is_file());
    assert!(!sandbox.slake_ledger().exists());

    // --db does not depend on HOME or the XDG variables.
    let unresolvable = sandbox.temp.path().join("no-home/ledger.sqlite3");
    Command::cargo_bin("slake")
        .unwrap()
        .env_remove("HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_DATA_HOME")
        .arg("--db")
        .arg(&unresolvable)
        .args(["status", "--name", "backup", "--min-interval", "1h"])
        .assert()
        .success()
        .stdout(contains("state=never-run"));
}
