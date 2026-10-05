# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/), and this project adheres to [Semantic Versioning](https://semver.org/).

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
