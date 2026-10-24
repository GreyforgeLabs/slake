use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::db;
use crate::model::{ClearResult, GuardState, RunRecord, RunResult, StatusResult};

const MAX_HISTORY_PER_JOB: i64 = 1000;
static CLAIM_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn parse_min_interval(value: &str) -> Result<Duration> {
    parse_positive_duration(value, "min interval")
}

pub fn parse_failure_backoff(value: &str) -> Result<Duration> {
    parse_positive_duration(value, "failure backoff")
}

pub fn parse_lease_duration(value: &str) -> Result<Duration> {
    parse_positive_duration(value, "lease")
}

fn parse_positive_duration(value: &str, label: &str) -> Result<Duration> {
    let duration =
        humantime::parse_duration(value).with_context(|| format!("invalid duration: {value}"))?;

    if duration.is_zero() {
        return Err(anyhow!("{label} must be greater than zero"));
    }
    if duration.as_nanos() % 1_000_000 != 0 {
        return Err(anyhow!("{label} must use whole-millisecond precision"));
    }
    i64::try_from(duration.as_millis())
        .with_context(|| format!("{label} is too large to store"))?;

    Ok(duration)
}

const LEDGER_FILE: &str = "runs.sqlite3";
const PROJECT_NAME: &str = "slake";
/// The tool's name before 0.4.0. Its ledger directories are still honoured so
/// that renaming the tool does not reset cooldowns or allow overlapping runs.
const LEGACY_PROJECT_NAME: &str = "cooldown-guard";

/// Resolves the ledger used when `--db` is not given.
///
/// The `slake` location wins whenever its ledger exists. Otherwise an existing
/// ledger written by `cooldown-guard` (0.3.0 and earlier) is used in place; it
/// is never copied or moved. With neither present, the `slake` location is used.
pub fn default_db_path() -> Result<PathBuf> {
    let project_dirs = ProjectDirs::from("tech", "Greyforge", PROJECT_NAME)
        .context("could not resolve a default state directory")?;
    let preferred = ledger_path(&project_dirs);
    let legacy = ProjectDirs::from("tech", "Greyforge", LEGACY_PROJECT_NAME)
        .map(|dirs| legacy_ledger_candidates(&dirs))
        .unwrap_or_default();

    Ok(resolve_db_path(preferred, &legacy))
}

fn ledger_path(project_dirs: &ProjectDirs) -> PathBuf {
    match project_dirs.state_dir() {
        Some(state_dir) => state_dir.join(LEDGER_FILE),
        None => project_dirs.data_local_dir().join(LEDGER_FILE),
    }
}

/// Every place `cooldown-guard` could have kept its ledger: the platform state
/// directory (Linux `$XDG_STATE_HOME/cooldown-guard/`) and the local data
/// directory (Linux `$XDG_DATA_HOME/cooldown-guard/`, macOS
/// `~/Library/Application Support/tech.Greyforge.cooldown-guard/`, Windows
/// `%LOCALAPPDATA%\Greyforge\cooldown-guard\data\`).
fn legacy_ledger_candidates(project_dirs: &ProjectDirs) -> Vec<PathBuf> {
    let mut candidates = Vec::with_capacity(2);
    if let Some(state_dir) = project_dirs.state_dir() {
        candidates.push(state_dir.join(LEDGER_FILE));
    }
    let data_local = project_dirs.data_local_dir().join(LEDGER_FILE);
    if !candidates.contains(&data_local) {
        candidates.push(data_local);
    }
    candidates
}

fn resolve_db_path(preferred: PathBuf, legacy: &[PathBuf]) -> PathBuf {
    if preferred.exists() {
        return preferred;
    }
    legacy
        .iter()
        .find(|candidate| candidate.is_file())
        .cloned()
        .unwrap_or(preferred)
}

pub fn open_database(path: &Path) -> Result<Connection> {
    db::open(path)
}

pub fn status(
    connection: &Connection,
    name: &str,
    min_interval: Duration,
    failure_backoff: Duration,
) -> Result<StatusResult> {
    validate_job_name(name)?;
    status_at(connection, name, min_interval, failure_backoff, now_ms())
}

pub fn run_guarded(
    connection: &mut Connection,
    name: &str,
    min_interval: Duration,
    failure_backoff: Duration,
    lease_duration: Duration,
    command: &[String],
) -> Result<RunResult> {
    validate_job_name(name)?;
    if command.is_empty() {
        return Err(anyhow!("missing command to execute"));
    }

    // Installed before the claim so that a stop request arriving at any point
    // reaches the child and the claim is still finalized.
    let forwarding = signals::Forwarding::install();
    let owner_token = claim_token(name);
    let claim = claim_run(
        connection,
        name,
        min_interval,
        failure_backoff,
        lease_duration,
        &owner_token,
    )?;
    if let ClaimOutcome::CoolingDown(current_status) = claim {
        return Ok(RunResult {
            name: name.to_owned(),
            action: "skip",
            skipped: true,
            exit_code: None,
            signal: None,
            last_exit_code: current_status.last_exit_code,
            started_at: None,
            finished_at: None,
            remaining_seconds: current_status.remaining_seconds,
        });
    }

    let started_at = now_ms();
    let status = Command::new(&command[0])
        .args(&command[1..])
        .spawn()
        .and_then(|mut child| {
            forwarding.child_started(child.id());
            let status = child.wait();
            forwarding.child_exited();
            status
        })
        .with_context(|| format!("failed to execute {:?}", command));
    let finished_at = now_ms();
    // `forwarding` stays installed until this function returns, so a stop
    // request that arrives while the result is recorded cannot interrupt the
    // finalize transaction and leave the claim held.

    let record = RunRecord {
        name: name.to_owned(),
        started_at,
        finished_at,
        exit_code: status
            .as_ref()
            .ok()
            .and_then(std::process::ExitStatus::code),
        succeeded: status.as_ref().is_ok_and(std::process::ExitStatus::success),
    };
    let signal = status.as_ref().ok().and_then(signals::terminating_signal);
    finalize_run(connection, &owner_token, &record).with_context(|| {
        format!(
            "{} was not recorded",
            match (record.exit_code, signal) {
                (Some(code), _) => format!("the command's exit code {code}"),
                (None, Some(signal)) => format!("the command's termination by signal {signal}"),
                (None, None) => "the spawn failure".to_owned(),
            }
        )
    })?;
    status?;

    Ok(RunResult {
        name: name.to_owned(),
        action: "run",
        skipped: false,
        exit_code: record.exit_code,
        signal,
        last_exit_code: None,
        started_at: Some(format_timestamp(started_at)?),
        finished_at: Some(format_timestamp(finished_at)?),
        remaining_seconds: None,
    })
}

#[derive(Debug)]
enum ClaimOutcome {
    Claimed,
    CoolingDown(StatusResult),
}

fn claim_run(
    connection: &mut Connection,
    name: &str,
    min_interval: Duration,
    failure_backoff: Duration,
    lease_duration: Duration,
    owner_token: &str,
) -> Result<ClaimOutcome> {
    let lease_ms = duration_ms_i64(lease_duration, "lease")?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = now_ms();
    tx.execute(
        "DELETE FROM run_claims WHERE lease_expires_at_ms <= ?",
        params![now],
    )?;
    rebase_future_timestamps(&tx, name, now)?;
    let current_status = status_at(&tx, name, min_interval, failure_backoff, now)?;
    if current_status.remaining_seconds.is_some() {
        // Commit, not roll back, so that expired claims and any clock-skew
        // rebase persist. With nothing changed the commit writes nothing.
        tx.commit()?;
        return Ok(ClaimOutcome::CoolingDown(current_status));
    }
    tx.execute(
        "
        INSERT INTO run_claims (name, owner_token, claimed_at_ms, lease_expires_at_ms)
        VALUES (?, ?, ?, ?)
        ",
        params![name, owner_token, now, now.saturating_add(lease_ms)],
    )?;
    tx.commit()?;
    Ok(ClaimOutcome::Claimed)
}

/// Repairs timestamps that lie in the future, which only happens after the wall
/// clock was set backwards. Left alone, such a row would block the job until
/// the clock caught up again, possibly for years; rebased to `now`, the job
/// waits exactly one interval (or one lease) from the moment the skew is seen.
fn rebase_future_timestamps(tx: &Connection, name: &str, now: i64) -> Result<()> {
    tx.execute(
        "
        UPDATE runs SET finished_at_ms = ?2, finished_at = ?2 / 1000,
            started_at_ms = min(started_at_ms, ?2), started_at = min(started_at, ?2 / 1000)
        WHERE name = ?1 AND finished_at_ms > ?2
        ",
        params![name, now],
    )?;
    tx.execute(
        "
        UPDATE run_claims
        SET lease_expires_at_ms = ?2 + (lease_expires_at_ms - claimed_at_ms), claimed_at_ms = ?2
        WHERE name = ?1 AND claimed_at_ms > ?2
        ",
        params![name, now],
    )?;
    Ok(())
}

fn finalize_run(connection: &mut Connection, owner_token: &str, record: &RunRecord) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = now_ms();
    let updated = tx.execute(
        "
        DELETE FROM run_claims
        WHERE name = ? AND owner_token = ? AND lease_expires_at_ms > ?
        ",
        params![record.name, owner_token, now],
    )?;
    if updated == 0 {
        tx.execute(
            "DELETE FROM run_claims WHERE name = ? AND lease_expires_at_ms <= ?",
            params![record.name, now],
        )?;
        tx.commit()?;
        return Err(anyhow!("run claim expired or was replaced before finalize"));
    }
    tx.execute(
        "
        INSERT INTO runs
            (name, started_at, finished_at, started_at_ms, finished_at_ms, exit_code, succeeded)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ",
        params![
            record.name,
            record.started_at / 1000,
            record.finished_at / 1000,
            record.started_at,
            record.finished_at,
            record.exit_code,
            if record.succeeded { 1 } else { 0 }
        ],
    )?;
    tx.execute(
        "
        DELETE FROM runs WHERE id IN (
            SELECT id FROM runs WHERE name = ?1
            ORDER BY finished_at_ms DESC, id DESC LIMIT -1 OFFSET ?2
        )
        ",
        params![record.name, MAX_HISTORY_PER_JOB],
    )?;
    tx.commit()?;
    Ok(())
}

fn status_at(
    connection: &Connection,
    name: &str,
    min_interval: Duration,
    failure_backoff: Duration,
    now: i64,
) -> Result<StatusResult> {
    let last = db::last_run(connection, name)?;
    let active_lease_expiry = connection
        .query_row(
            "SELECT lease_expires_at_ms FROM run_claims WHERE name = ? AND lease_expires_at_ms > ?",
            params![name, now],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;

    let lease_remaining = active_lease_expiry
        .map(|expires_at| remaining_ms_as_seconds(expires_at.saturating_sub(now) as u64));

    let (
        record_remaining,
        last_exit_code,
        last_succeeded,
        last_started_at,
        last_finished_at,
        elapsed_seconds,
    ) = match last {
        None => (None, None, None, None, None, None),
        Some(record) => {
            let elapsed = elapsed_ms(now, record.finished_at);
            let interval = if record.succeeded {
                min_interval
            } else {
                failure_backoff
            };
            (
                remaining_seconds(elapsed, interval),
                record.exit_code,
                Some(record.succeeded),
                Some(format_timestamp(record.started_at)?),
                Some(format_timestamp(record.finished_at)?),
                Some(elapsed / 1000),
            )
        }
    };
    let remaining_seconds = match (record_remaining, lease_remaining) {
        (Some(record), Some(lease)) => Some(record.max(lease)),
        (Some(record), None) => Some(record),
        (None, Some(lease)) => Some(lease),
        (None, None) => None,
    };
    let state = if remaining_seconds.is_some() {
        GuardState::CoolingDown
    } else if last_succeeded.is_none() {
        GuardState::NeverRun
    } else {
        GuardState::Ready
    };

    Ok(StatusResult {
        name: name.to_owned(),
        state,
        last_exit_code,
        last_succeeded,
        last_started_at,
        last_finished_at,
        elapsed_seconds,
        remaining_seconds,
    })
}

pub fn clear(connection: &mut Connection, name: &str, force: bool) -> Result<ClearResult> {
    validate_job_name(name)?;
    let deleted_runs = db::clear_runs(connection, name, force, now_ms())?;
    Ok(ClearResult {
        name: name.to_owned(),
        deleted_runs,
    })
}

pub fn format_duration(seconds: u64) -> String {
    humantime::format_duration(Duration::from_secs(seconds)).to_string()
}

fn elapsed_ms(now: i64, finished_at: i64) -> u64 {
    now.saturating_sub(finished_at).max(0) as u64
}

fn remaining_seconds(elapsed_ms: u64, min_interval: Duration) -> Option<u64> {
    let min_interval_ms = duration_ms_u64(min_interval);
    if elapsed_ms < min_interval_ms {
        Some(remaining_ms_as_seconds(min_interval_ms - elapsed_ms))
    } else {
        None
    }
}

fn remaining_ms_as_seconds(remaining_ms: u64) -> u64 {
    remaining_ms.div_ceil(1000)
}

fn duration_ms_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn duration_ms_i64(duration: Duration, label: &str) -> Result<i64> {
    i64::try_from(duration.as_millis()).with_context(|| format!("{label} is too large to store"))
}

/// Formats whole seconds of a millisecond Unix time as RFC 3339 UTC, for
/// example `2026-04-07T15:16:39Z`.
fn format_timestamp(unix_ms: i64) -> Result<String> {
    let seconds = unix_ms / 1000;
    let (days, second_of_day) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Civil-from-days conversion (proleptic Gregorian calendar).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    if !(0..=9999).contains(&year) {
        return Err(anyhow!(
            "timestamp {unix_ms} ms is outside the RFC 3339 year range"
        ));
    }
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3600,
        second_of_day / 60 % 60,
        second_of_day % 60
    ))
}

fn now_ms() -> i64 {
    let millis = |duration: Duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => millis(since),
        Err(before) => -millis(before.duration()),
    }
}

/// Called after a run's result is printed. See `signals::end_like_command`.
pub fn end_like_command(signal: i32) {
    signals::end_like_command(signal);
}

pub fn validate_job_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 128 {
        return Err(anyhow!("job name must be 1..128 ASCII characters"));
    }
    let mut bytes = name.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || bytes
            .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(anyhow!(
            "job name must start with an ASCII letter or digit and contain only letters, digits, '.', '_', ':', or '-'"
        ));
    }
    Ok(())
}

fn claim_token(_name: &str) -> String {
    let sequence = CLAIM_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{}:{nanos}:{sequence}", std::process::id())
}

/// Stop requests (SIGHUP, SIGINT, SIGQUIT, SIGTERM) are relayed to the child
/// while it runs, so slake outlives it, records the result and releases the
/// claim. Without this, `kill`, `systemctl stop` or a shutdown would end slake
/// first and leave the claim blocking the job for the whole lease.
#[cfg(unix)]
mod signals {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::sync::atomic::{AtomicI32, Ordering};

    const RELAYED: [libc::c_int; 4] = [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];
    static CHILD: AtomicI32 = AtomicI32::new(0);
    static PENDING: AtomicI32 = AtomicI32::new(0);

    extern "C" fn relay(signal: libc::c_int, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
        let child = CHILD.load(Ordering::SeqCst);
        if child == 0 {
            // No child yet: hand the request to it as soon as it exists.
            PENDING.store(signal, Ordering::SeqCst);
        } else if unsafe { (*info).si_code } <= 0 {
            // Sent by a process (kill, systemd). A terminal-generated signal
            // (si_code > 0) already reached the child through its process group.
            unsafe { libc::kill(child, signal) };
        }
    }

    pub struct Forwarding {
        previous: [libc::sigaction; RELAYED.len()],
    }

    impl Forwarding {
        /// Installs the relay for every stop signal that is not ignored. A
        /// signal the caller ignores (for example SIGHUP under `nohup`) stays
        /// ignored, so the child keeps inheriting that disposition.
        pub fn install() -> Self {
            // SAFETY: plain sigaction calls with a zeroed, fully initialised
            // struct; the handler only touches atomics and calls kill(2).
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                let handler: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                    relay;
                action.sa_sigaction = handler as libc::sighandler_t;
                action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
                libc::sigemptyset(&mut action.sa_mask);
                let mut previous: [libc::sigaction; RELAYED.len()] = std::mem::zeroed();
                for (signal, slot) in RELAYED.iter().zip(previous.iter_mut()) {
                    libc::sigaction(*signal, std::ptr::null(), slot);
                    if slot.sa_sigaction != libc::SIG_IGN {
                        libc::sigaction(*signal, &action, std::ptr::null_mut());
                    }
                }
                Self { previous }
            }
        }

        pub fn child_started(&self, pid: u32) {
            let pid = pid as i32;
            CHILD.store(pid, Ordering::SeqCst);
            let pending = PENDING.swap(0, Ordering::SeqCst);
            if pending != 0 {
                unsafe { libc::kill(pid, pending) };
            }
        }

        /// Called once the child is reaped, before its PID can be reused.
        pub fn child_exited(&self) {
            CHILD.store(0, Ordering::SeqCst);
        }
    }

    impl Drop for Forwarding {
        fn drop(&mut self) {
            self.child_exited();
            PENDING.store(0, Ordering::SeqCst);
            for (signal, previous) in RELAYED.iter().zip(self.previous.iter()) {
                unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) };
            }
        }
    }

    pub fn terminating_signal(status: &ExitStatus) -> Option<i32> {
        status.signal()
    }

    /// Ends slake by the signal that ended the command, for SIGHUP, SIGINT and
    /// SIGTERM. Shells still see `128 + N`, but supervisors see a signal death
    /// as they would without slake: systemd counts a stop as clean, and a shell
    /// script interrupted with Ctrl-C stops instead of carrying on. Returns
    /// when the signal is ignored or blocked; the caller then exits `128 + N`.
    pub fn end_like_command(signal: i32) {
        if [libc::SIGHUP, libc::SIGINT, libc::SIGTERM].contains(&signal) {
            // SAFETY: raise(3) with a valid signal number.
            unsafe { libc::raise(signal) };
        }
    }
}

#[cfg(not(unix))]
mod signals {
    use std::process::ExitStatus;

    pub struct Forwarding;

    impl Forwarding {
        pub fn install() -> Self {
            Self
        }
        pub fn child_started(&self, _pid: u32) {}
        pub fn child_exited(&self) {}
    }

    pub fn terminating_signal(_status: &ExitStatus) -> Option<i32> {
        None
    }

    pub fn end_like_command(_signal: i32) {}
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;

    use rusqlite::params;
    use tempfile::TempDir;

    use crate::db;
    use crate::model::RunRecord;

    use super::{
        ClaimOutcome, MAX_HISTORY_PER_JOB, claim_run, finalize_run, format_timestamp,
        parse_min_interval, remaining_seconds, resolve_db_path, run_guarded, validate_job_name,
    };

    #[test]
    fn timestamps_format_as_rfc3339_utc() {
        for (unix_ms, expected) in [
            (0, "1970-01-01T00:00:00Z"),
            (999, "1970-01-01T00:00:00Z"),
            (-1000, "1969-12-31T23:59:59Z"),
            (31_536_000_000, "1971-01-01T00:00:00Z"),
            (951_782_400_000, "2000-02-29T00:00:00Z"),
            (1_775_574_999_000, "2026-04-07T15:16:39Z"),
            (4_107_542_399_000, "2100-02-28T23:59:59Z"),
            (4_107_542_400_000, "2100-03-01T00:00:00Z"),
            (-62_167_219_200_000, "0000-01-01T00:00:00Z"),
            (253_402_300_799_000, "9999-12-31T23:59:59Z"),
        ] {
            assert_eq!(format_timestamp(unix_ms).unwrap(), expected, "{unix_ms}");
        }
        assert!(format_timestamp(253_402_300_800_000).is_err());
        assert!(format_timestamp(-62_167_219_201_000).is_err());
    }

    #[test]
    fn claim_rebases_timestamps_left_in_the_future_by_a_clock_change() {
        let temp = TempDir::new().expect("tempdir");
        let mut connection = db::open(&temp.path().join("runs.sqlite3")).expect("database");
        let future = super::now_ms() + 10 * 365 * 86_400_000;
        connection
            .execute(
                "
                INSERT INTO runs
                    (name, started_at, finished_at, started_at_ms, finished_at_ms, exit_code, succeeded)
                VALUES ('skewed', ?1 / 1000, ?1 / 1000, ?1, ?1, 0, 1)
                ",
                params![future],
            )
            .expect("future row");
        let claim = |connection: &mut rusqlite::Connection| {
            claim_run(
                connection,
                "skewed",
                Duration::from_millis(50),
                Duration::from_millis(50),
                Duration::from_secs(60),
                "owner",
            )
            .expect("claim")
        };
        // The skewed row is rebased to now, so one interval still applies ...
        assert!(matches!(
            claim(&mut connection),
            ClaimOutcome::CoolingDown(_)
        ));
        let finished: i64 = connection
            .query_row(
                "SELECT finished_at_ms FROM runs WHERE name = 'skewed'",
                [],
                |row| row.get(0),
            )
            .expect("rebased row");
        assert!(finished <= super::now_ms());
        // ... and only one interval, not ten years.
        thread::sleep(Duration::from_millis(80));
        assert!(matches!(claim(&mut connection), ClaimOutcome::Claimed));
    }

    #[test]
    fn default_ledger_falls_back_to_existing_legacy_ledger() {
        let temp = TempDir::new().expect("tempdir");
        let preferred = temp.path().join("slake").join("runs.sqlite3");
        let legacy_state = temp.path().join("state").join("runs.sqlite3");
        let legacy_data = temp.path().join("data").join("runs.sqlite3");
        let legacy = [legacy_state.clone(), legacy_data.clone()];

        assert_eq!(resolve_db_path(preferred.clone(), &legacy), preferred);

        std::fs::create_dir_all(legacy_data.parent().unwrap()).unwrap();
        std::fs::write(&legacy_data, b"").unwrap();
        assert_eq!(resolve_db_path(preferred.clone(), &legacy), legacy_data);

        std::fs::create_dir_all(legacy_state.parent().unwrap()).unwrap();
        std::fs::write(&legacy_state, b"").unwrap();
        assert_eq!(resolve_db_path(preferred.clone(), &legacy), legacy_state);

        std::fs::create_dir_all(preferred.parent().unwrap()).unwrap();
        std::fs::write(&preferred, b"").unwrap();
        assert_eq!(resolve_db_path(preferred.clone(), &legacy), preferred);
    }

    #[test]
    fn legacy_directory_without_ledger_is_ignored() {
        let temp = TempDir::new().expect("tempdir");
        let preferred = temp.path().join("slake").join("runs.sqlite3");
        let legacy = temp.path().join("cooldown-guard").join("runs.sqlite3");
        std::fs::create_dir_all(&legacy).unwrap();
        assert_eq!(resolve_db_path(preferred.clone(), &[legacy]), preferred);
    }

    #[test]
    fn parse_duration_requires_positive_value() {
        assert!(parse_min_interval("15m").is_ok());
        assert!(parse_min_interval("1ms").is_ok());
        assert!(parse_min_interval("999ms").is_ok());
        assert!(parse_min_interval("1s").is_ok());
        assert!(parse_min_interval("1us").is_err());
        assert!(parse_min_interval("0s").is_err());
    }

    #[test]
    fn cooldown_remaining_is_none_after_interval() {
        assert_eq!(remaining_seconds(120_000, Duration::from_secs(60)), None);
        assert_eq!(remaining_seconds(30_000, Duration::from_secs(60)), Some(30));
        assert_eq!(remaining_seconds(0, Duration::from_millis(1)), Some(1));
        assert_eq!(remaining_seconds(1, Duration::from_millis(999)), Some(1));
        assert_eq!(remaining_seconds(999, Duration::from_millis(999)), None);
        assert_eq!(remaining_seconds(999, Duration::from_secs(1)), Some(1));
    }

    #[test]
    fn job_names_are_normalized_and_bounded() {
        for name in [
            "backup",
            "backup.daily",
            "backup_daily",
            "backup:daily",
            "9-job",
        ] {
            assert!(validate_job_name(name).is_ok(), "{name}");
        }
        for name in [
            "",
            " backup",
            "backup job",
            "backup/job",
            "éclair",
            "-backup",
        ] {
            assert!(validate_job_name(name).is_err(), "{name}");
        }
        assert!(validate_job_name(&"a".repeat(128)).is_ok());
        assert!(validate_job_name(&"a".repeat(129)).is_err());
    }

    #[test]
    fn expired_claim_is_recoverable_and_stale_owner_cannot_finalize() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("runs.sqlite3");
        let mut connection = db::open(&path).expect("database");

        assert!(matches!(
            claim_run(
                &mut connection,
                "backup",
                Duration::from_secs(60),
                Duration::from_secs(5),
                Duration::from_millis(1),
                "old-owner",
            )
            .expect("first claim"),
            ClaimOutcome::Claimed
        ));
        thread::sleep(Duration::from_millis(5));
        assert!(matches!(
            claim_run(
                &mut connection,
                "backup",
                Duration::from_secs(60),
                Duration::from_secs(5),
                Duration::from_secs(60),
                "new-owner",
            )
            .expect("replacement claim"),
            ClaimOutcome::Claimed
        ));

        let record = RunRecord {
            name: "backup".to_owned(),
            started_at: super::now_ms(),
            finished_at: super::now_ms(),
            exit_code: Some(0),
            succeeded: true,
        };
        assert!(finalize_run(&mut connection, "old-owner", &record).is_err());
        finalize_run(&mut connection, "new-owner", &record).expect("current owner finalizes");
    }

    #[test]
    fn expired_crash_claim_recovers_after_database_reopen() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("runs.sqlite3");
        {
            let mut connection = db::open(&path).expect("database");
            assert!(matches!(
                claim_run(
                    &mut connection,
                    "backup",
                    Duration::from_secs(60),
                    Duration::from_secs(5),
                    Duration::from_millis(1),
                    "crashed-owner",
                )
                .expect("claim"),
                ClaimOutcome::Claimed
            ));
        }
        thread::sleep(Duration::from_millis(5));

        let mut reopened = db::open(&path).expect("reopened database");
        assert!(matches!(
            claim_run(
                &mut reopened,
                "backup",
                Duration::from_secs(60),
                Duration::from_secs(5),
                Duration::from_secs(60),
                "recovery-owner",
            )
            .expect("recovery claim"),
            ClaimOutcome::Claimed
        ));
    }

    #[test]
    fn finalized_runs_prune_history_to_policy_limit() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("runs.sqlite3");
        let mut connection = db::open(&path).expect("database");
        for id in 0..(MAX_HISTORY_PER_JOB + 5) {
            connection
                .execute(
                    "
                    INSERT INTO runs
                        (name, started_at, finished_at, started_at_ms, finished_at_ms, exit_code, succeeded)
                    VALUES ('retained', 0, 0, ?, ?, 0, 1)
                    ",
                    params![id, id],
                )
                .expect("history row");
        }

        let command = vec!["true".to_owned()];
        run_guarded(
            &mut connection,
            "retained",
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_secs(30),
            &command,
        )
        .expect("guarded run");
        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM runs WHERE name = 'retained'",
                [],
                |row| row.get(0),
            )
            .expect("history count");
        assert_eq!(count, MAX_HISTORY_PER_JOB);
    }

    #[test]
    fn database_busy_wait_is_bounded_and_reports_lock_error() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("runs.sqlite3");
        let blocker = db::open(&path).expect("blocker database");
        let mut contender = db::open(&path).expect("contender database");
        contender
            .busy_timeout(Duration::from_millis(50))
            .expect("test busy timeout");
        blocker
            .execute_batch("BEGIN IMMEDIATE")
            .expect("write lock");

        let started = Instant::now();
        let error = claim_run(
            &mut contender,
            "busy-job",
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            "contender",
        )
        .expect_err("claim should report the lock");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(error.to_string().contains("database is locked"));
        blocker.execute_batch("ROLLBACK").expect("unlock database");
    }
}
