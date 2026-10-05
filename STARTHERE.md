# STARTHERE.md - Coding Client Bootstrap Guide

> This file is designed for coding assistants. If you are a human,
> see [README.md](README.md) for the human-friendly guide.

## Quick Bootstrap

```bash
git clone https://github.com/GreyforgeLabs/slake.git && cd slake && ./scripts/setup.sh
```

## What This Project Does

`slake` (released as `cooldown-guard` up to v0.3.0) is a Rust CLI for minimum-interval enforcement. It records completed command runs in SQLite, atomically leases named jobs without holding a transaction during execution, and applies separate success and failure cooldowns.

## Project Structure

```text
slake/
  src/
    main.rs              # `slake` process entry point
    lib.rs               # implementation shared by both binaries (not a stable API)
    bin/
      cooldown-guard.rs  # deprecated alias: stderr note, then identical behaviour
    cli.rs               # clap command definitions and rendering
    db.rs                # SQLite schema and persistence helpers
    guard.rs             # cooldown logic and command execution
    model.rs             # shared data structures
  tests/
    cli.rs               # integration tests for run/status/clear
  scripts/
    setup.sh             # idempotent build and verification script
  .github/workflows/
    ci.yml               # fmt, clippy, test, and MSRV (1.88) workflow
  README.md              # human-facing docs
  STARTHERE.md           # this file
```

## Setup Prerequisites

- Rust 1.88+
- `cargo`
- `rustfmt` component (`rustup component add rustfmt`)

## Installation Steps

1. Clone: `git clone https://github.com/GreyforgeLabs/slake.git`
2. Enter directory: `cd slake`
3. Run setup: `./scripts/setup.sh`

## Verification

```bash
cargo run --locked -- --version
# Expected output: slake 0.4.0
```

## Key Entry Points

- `src/cli.rs` - subcommands and output formatting
- `src/guard.rs` - cooldown evaluation, command execution, and default ledger resolution
- `src/db.rs` - SQLite schema and run history queries

## Configuration

- Default state DB: platform state directory for `slake`, usually `~/.local/state/slake/runs.sqlite3` on Linux
- Legacy ledger: if the slake DB does not exist but a `cooldown-guard` ledger does (`~/.local/state/cooldown-guard/runs.sqlite3`, `~/.local/share/cooldown-guard/runs.sqlite3`, or macOS `~/Library/Application Support/tech.Greyforge.cooldown-guard/runs.sqlite3`), it is used in place and never copied; see `default_db_path` in `src/guard.rs`
- Deprecated alias: the `cooldown-guard` binary prints one stderr line and then behaves exactly like `slake`; it is removed in the next release
- Override state DB: `--db /path/to/runs.sqlite3`
- Output mode: add `--json`
- Failure retry interval: `--failure-backoff 5m` (defaults to `--min-interval`)
- Claim lifetime: `--lease 24h` is fixed and nonrenewing; set it longer than the maximum expected command runtime. After expiry, overlap is possible.
- `clear` refuses an active claim; `clear --force` abandons one without stopping its child.
- Job names: 1–128 normalized ASCII characters; first character must be alphanumeric

## Common Tasks

```bash
# Run tests
cargo test --locked

# Format check
cargo fmt --check

# Lint
cargo clippy --all-targets --all-features -- -D warnings

# Run an example command
cargo run -- run --name backup --min-interval 30m -- ./backup.sh
```
