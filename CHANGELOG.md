# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- A stop signal (SIGHUP, SIGINT, SIGQUIT or SIGTERM) that arrives before the command starts, for example while slake waits for the ledger lock, now releases the claim and starts nothing. Before, slake started the command, killed it at once with the queued signal and recorded a failure, which put the job into failure backoff. slake reports `action=stop reason=signal signal=N` (JSON: `"action": "stop"`, `"skipped": true`, `"signal": N`) and ends like a signal-terminated run (`128 + N`, or death by the same signal for SIGHUP, SIGINT and SIGTERM).
- Durations and the job name are validated before the ledger is opened. A mistyped invocation no longer creates the ledger file and its directory, and no longer waits on a locked ledger before it reports the usage error.

## [0.5.0] - 2026-10-14

### Added

- Stop signals (SIGHUP, SIGINT, SIGQUIT, SIGTERM) sent to slake while the command runs are relayed to the command. slake waits for it, records the result and releases the claim, so `kill`, `systemctl stop` or a shutdown no longer leaves the job blocked for the rest of its lease. Signals the caller ignores, such as SIGHUP under `nohup`, stay ignored for the command.
- Run results report the terminating signal: `signal=N` in text output and a `"signal"` field (a number or `null`) in JSON output.
- A tag-triggered release workflow publishes a Linux x86_64 tarball with a SHA-256 checksum.

### Changed

- A command terminated by signal `N` now makes slake exit with `128 + N` (for example `137` for SIGKILL), the convention shells use, instead of `1`. For SIGHUP, SIGINT and SIGTERM, slake records and prints the result and then ends by the same signal: shells still see `128 + N` (`143` for SIGTERM), systemd counts a `systemctl stop` as a clean stop, and Ctrl-C stops a calling shell script.
- The ledger records its schema revision in `PRAGMA user_version`. Older ledgers are upgraded once, in one immediate transaction, and later invocations skip the schema checks: one statement on open instead of seven, and no full-table scan per invocation. `status` and cooldown skips on a 50,000-row ledger run in 1.5 ms instead of 7.7–8.5 ms.
- The unused seconds-precision index is dropped, and the millisecond index now also covers `id`, so newest-first reads and history pruning walk the index without sorting.
- The bundled SQLite is built without features slake does not use (FTS, R*Tree, JSON, extension loading and others). The release binary is 27% smaller: 2.91 MB to 2.13 MB.
- `serde`, `serde_json` and `time` are replaced by small built-in formatters with identical output, and clap no longer pulls in its color stack: the runtime dependency tree shrinks from 45 to 24 crates. Peak memory for a `status` call drops from 5.1 MB to 4.0 MB.

### Fixed

- After the wall clock is set backwards, runs and claims that lie in the future are rebased to the current time on the next `run`, so the job waits one interval or lease instead of until the clock catches up.
- A failure to record a result now says which outcome (exit code, signal or spawn failure) was not recorded, and a closed stdout is reported as an error instead of aborting.

### Compatibility

- Text output, JSON fields and exit codes are unchanged apart from the additions above. Ledgers stay readable and writable by earlier releases.
- The deprecated `cooldown-guard` alias is still installed and will be removed in a future release.

## [0.4.0] - 2026-10-05

### Changed

- Renamed from `cooldown-guard` to `slake`: crate, binary, `--version` output and repository (`GreyforgeLabs/slake`). Releases up to 0.3.0 were published as `cooldown-guard`.
- Release builds use fat LTO, a single codegen unit, symbol stripping and abort-on-panic. The x86_64 Linux release binary shrinks from 3.68 MB to 2.91 MB (-21%) with unchanged startup time and test results.
- The default ledger path is resolved only when `--db` is omitted, so an explicit `--db` no longer depends on a resolvable home directory.

### Deprecated

- The `cooldown-guard` binary is installed as an alias for this release only. It prints a one-line deprecation note to stderr and then behaves exactly like `slake` (same arguments, stdout and exit codes). It will be removed in the next release; switch cron lines and scripts to `slake`.

### Compatibility

- Existing ledgers keep working. Without `--db`, slake uses its own ledger (`~/.local/state/slake/runs.sqlite3` on Linux) when it exists; otherwise it uses an existing `cooldown-guard` ledger in place: the Linux state directory (`$XDG_STATE_HOME/cooldown-guard/`, default `~/.local/state/cooldown-guard/`), the local data directory (`$XDG_DATA_HOME/cooldown-guard/`) or, on macOS, `~/Library/Application Support/tech.Greyforge.cooldown-guard/`. The legacy ledger is never copied or moved, so cooldowns and active claims carry over without a reset or an overlap window. With no ledger anywhere, a new one is created at the slake location. The SQLite schema is unchanged.

## [0.3.0] - 2026-09-27

### Fixed

- `clear` now checks and deletes state in one transaction, refusing an active claim unless `--force` is supplied.

### Changed

- Explain explicitly that a fixed lease limits overlap protection to its lifetime; a running child can outlive it.
- Document that forced clear abandons a claim without stopping its child.

## [0.2.0] - 2026-08-28

### Added

- Short owner-token claim leases with configurable expiry and crash recovery
- Configurable failure backoff for spawn failures and nonzero command exits
- Strict normalized job names and a 1,000-row per-job retention policy
- Regression coverage for contention, stale owners, crash recovery, bounded database waits, failure retries, subsecond durations, and legacy migration

### Changed

- Run child commands outside SQLite write transactions so unrelated jobs can execute concurrently
- Store timestamps at whole-millisecond precision while migrating existing v0.1 rows in place
- Report active leases through status and reject stale owners during finalization

## [0.1.0] - 2026-04-07

### Added

- Initial Rust CLI for minimum-interval command enforcement
- SQLite-backed run history with `run`, `status`, and `clear` subcommands
- JSON output mode plus human-readable shell output
- Integration tests and GitHub Actions CI
- README, STARTHERE bootstrap, and setup script
