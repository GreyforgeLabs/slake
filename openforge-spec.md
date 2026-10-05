# slake - Implementation Spec

**Status:** Released
**Pipeline:** `forge openforge slake`
**License:** AGPL-3.0
**Repo:** `github.com/GreyforgeLabs/slake`
**Version:** v0.4.0
**Formerly:** `cooldown-guard` (released under that name up to v0.3.0; the `cooldown-guard` binary remains as a deprecated alias for v0.4.0 only)
**Language:** Rust

---

## 1. What It Is

A small Rust CLI for minimum-interval enforcement.

It solves the gap between:

- overlap locks like `flock`, which stop concurrent execution but do not enforce cadence
- full scheduler platforms, which are far heavier than needed when the only rule is "do not run this again yet"

**Audience:** operators, homelab users, cron-heavy environments, repair loops, and maintenance pipelines.

## 2. v0.2 Scope

The initial release stays narrow:

- `run` executes a command only if the cooldown has elapsed
- `status` reports whether a named key is ready or still cooling down
- `clear` removes saved state for a key, refusing an active claim unless `--force` is supplied
- SQLite-backed run history
- human-readable and JSON output
- whole-millisecond cooldown precision
- short atomic claim leases with crash recovery
- configurable failure backoff and bounded per-job history

Successful attempts use `--min-interval`. Spawn failures and nonzero exits use
`--failure-backoff`, which defaults to the minimum interval for compatibility.

## 3. Architecture

### 3.1 State Model

- one SQLite database
- one append-only `runs` history table
- one `run_claims` table keyed by normalized job `name`
- each row stores:
  - `name`
  - `started_at`
  - `finished_at`
  - `exit_code`
  - `succeeded`
- each claim stores:
  - an opaque owner token
  - claim time in milliseconds
  - lease expiry in milliseconds
- existing v0.1 second-precision rows migrate additively
- only the newest 1,000 completed attempts per job are retained

### 3.2 Execution Model

1. resolve DB path: `--db` if given; otherwise the slake ledger if it exists,
   else an existing legacy `cooldown-guard` ledger used in place (never copied),
   else the slake ledger location
2. in a short immediate transaction, remove expired claims and compare the
   latest run or active claim to `now`
3. if the job is ready, atomically write an owner-token lease and commit
4. if the remaining cooldown or active lease is greater than zero:
   - print skip result
   - exit `0`
5. otherwise:
   - execute the provided command directly, outside any write transaction
   - in a second short transaction, record the result only if the unexpired
     owner token still matches
   - return the child exit code

An expired claim can be replaced without database repair. A stale process may
finish its child command, but it cannot finalize after lease expiry or
replacement. The lease is not renewed: overlapping child execution is possible
after expiry, so the lease must exceed the expected child runtime. `clear`
refuses a live claim unless `--force` explicitly accepts that same risk.
SQLite busy waits are bounded to five seconds and returned as
observable runtime errors.

### 3.3 Security Boundary

- `slake` does not interpolate user input through an internal shell
- it executes the exact program and arguments supplied by the caller
- SQLite is local-only state, not a network service
- no internal Greyforge paths, recipe names, or host identifiers are carried into the public repo

## 4. Public CLI

```bash
slake run --name backup --min-interval 30m -- ./backup.sh
slake run --name backup --min-interval 30m --failure-backoff 5m --lease 2h -- ./backup.sh
slake status --name backup --min-interval 30m
slake --json status --name backup --min-interval 30m
slake clear --name backup
```

## 5. Release Surface

- `README.md`
- `STARTHERE.md`
- `scripts/setup.sh`
- GitHub Actions CI
- integration tests for `run`, `status`, and `clear`

## 6. Deferred Work

Deliberately not in v0.2:

- success-only cooldown policies
- labels/tags per key
- subcommands for listing all tracked keys
- shell-completion generation
