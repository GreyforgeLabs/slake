use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, anyhow};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::model::RunRecord;

/// Schema revision recorded in `PRAGMA user_version`. Ledgers written by 0.4.0
/// and earlier report 0 and are migrated once, in place, on first open.
const SCHEMA_VERSION: i64 = 1;

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    if schema_version(&connection)? < SCHEMA_VERSION {
        migrate(&mut connection)?;
    }
    Ok(connection)
}

fn schema_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row("PRAGMA user_version", [], |row| row.get(0))
}

/// Creates or upgrades the schema inside one immediate transaction, so
/// concurrent first opens of a legacy ledger cannot race on `ALTER TABLE`.
fn migrate(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if schema_version(&tx)? >= SCHEMA_VERSION {
        return Ok(());
    }
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS runs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            started_at INTEGER NOT NULL,
            finished_at INTEGER NOT NULL,
            started_at_ms INTEGER NOT NULL DEFAULT 0,
            finished_at_ms INTEGER NOT NULL DEFAULT 0,
            exit_code INTEGER,
            succeeded INTEGER NOT NULL CHECK (succeeded IN (0, 1))
        );
        CREATE TABLE IF NOT EXISTS run_claims (
            name TEXT PRIMARY KEY,
            owner_token TEXT NOT NULL,
            claimed_at_ms INTEGER NOT NULL,
            lease_expires_at_ms INTEGER NOT NULL
        );
        ",
    )?;
    let columns = tx
        .prepare("PRAGMA table_info(runs)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for column in ["started_at_ms", "finished_at_ms"] {
        if !columns.iter().any(|name| name == column) {
            tx.execute_batch(&format!(
                "ALTER TABLE runs ADD COLUMN {column} INTEGER NOT NULL DEFAULT 0"
            ))?;
        }
    }
    // v0.1 rows carry whole seconds only. The seconds index is unused since
    // v0.2; the millisecond index gains `id` so newest-first reads and history
    // pruning walk the index without a sort.
    tx.execute_batch(
        "
        UPDATE runs SET started_at_ms = started_at * 1000 WHERE started_at_ms = 0;
        UPDATE runs SET finished_at_ms = finished_at * 1000 WHERE finished_at_ms = 0;
        DROP INDEX IF EXISTS idx_runs_name_finished_at;
        DROP INDEX IF EXISTS idx_runs_name_finished_at_ms;
        CREATE INDEX idx_runs_name_finished_at_ms ON runs(name, finished_at_ms DESC, id DESC);
        ",
    )?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

pub fn last_run(connection: &Connection, name: &str) -> Result<Option<RunRecord>> {
    let row = connection
        .query_row(
            "
            SELECT name, started_at_ms, finished_at_ms, exit_code, succeeded
            FROM runs
            WHERE name = ?
            ORDER BY finished_at_ms DESC, id DESC
            LIMIT 1
            ",
            params![name],
            |row| {
                Ok(RunRecord {
                    name: row.get(0)?,
                    started_at: row.get(1)?,
                    finished_at: row.get(2)?,
                    exit_code: row.get(3)?,
                    succeeded: row.get::<_, i64>(4)? != 0,
                })
            },
        )
        .optional()?;

    Ok(row)
}

pub fn clear_runs(
    connection: &mut Connection,
    name: &str,
    force: bool,
    now_ms: i64,
) -> Result<usize> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let active = tx
        .query_row(
            "SELECT 1 FROM run_claims WHERE name = ? AND lease_expires_at_ms > ?",
            params![name, now_ms],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if active && !force {
        return Err(anyhow!(
            "job has an active claim; use clear --force only if overlap is acceptable"
        ));
    }
    let deleted = tx.execute("DELETE FROM runs WHERE name = ?", params![name])?;
    tx.execute("DELETE FROM run_claims WHERE name = ?", params![name])?;
    tx.commit()?;
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::{SCHEMA_VERSION, last_run, open, schema_version};

    #[test]
    fn open_upgrades_a_0_4_ledger_once_and_keeps_its_rows() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("v0_4.sqlite3");
        let legacy = Connection::open(&path).expect("legacy database");
        legacy
            .execute_batch(
                "
                CREATE TABLE runs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    started_at INTEGER NOT NULL,
                    finished_at INTEGER NOT NULL,
                    started_at_ms INTEGER NOT NULL DEFAULT 0,
                    finished_at_ms INTEGER NOT NULL DEFAULT 0,
                    exit_code INTEGER,
                    succeeded INTEGER NOT NULL CHECK (succeeded IN (0, 1))
                );
                CREATE TABLE run_claims (
                    name TEXT PRIMARY KEY,
                    owner_token TEXT NOT NULL,
                    claimed_at_ms INTEGER NOT NULL,
                    lease_expires_at_ms INTEGER NOT NULL
                );
                CREATE INDEX idx_runs_name_finished_at ON runs(name, finished_at DESC);
                CREATE INDEX idx_runs_name_finished_at_ms ON runs(name, finished_at_ms DESC);
                INSERT INTO runs
                    (name, started_at, finished_at, started_at_ms, finished_at_ms, exit_code, succeeded)
                VALUES ('job', 1, 2, 1500, 2500, 3, 0);
                INSERT INTO run_claims VALUES ('job', 'owner', 1, 9223372036854775807);
                ",
            )
            .expect("0.4 schema");
        drop(legacy);

        let migrated = open(&path).expect("migrated database");
        assert_eq!(schema_version(&migrated).unwrap(), SCHEMA_VERSION);
        let record = last_run(&migrated, "job").unwrap().expect("row kept");
        assert_eq!((record.started_at, record.finished_at), (1500, 2500));
        assert_eq!(record.exit_code, Some(3));
        let claims: i64 = migrated
            .query_row("SELECT count(*) FROM run_claims", [], |row| row.get(0))
            .unwrap();
        assert_eq!(claims, 1, "active claims survive the upgrade");
        let indexes = migrated
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'runs'")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            indexes
                .iter()
                .all(|name| name != "idx_runs_name_finished_at")
        );
        let index_columns = migrated
            .prepare("PRAGMA index_info(idx_runs_name_finished_at_ms)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(2))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(index_columns, ["name", "finished_at_ms", "id"]);
        drop(migrated);

        // A current ledger is opened without re-running the migration.
        let reopened = open(&path).expect("reopened database");
        assert_eq!(schema_version(&reopened).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn open_migrates_v0_1_second_precision_rows() {
        let temp = TempDir::new().expect("tempdir");
        let path = temp.path().join("legacy.sqlite3");
        let legacy = Connection::open(&path).expect("legacy database");
        legacy
            .execute_batch(
                "
                CREATE TABLE runs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    started_at INTEGER NOT NULL,
                    finished_at INTEGER NOT NULL,
                    exit_code INTEGER,
                    succeeded INTEGER NOT NULL CHECK (succeeded IN (0, 1))
                );
                INSERT INTO runs
                    (name, started_at, finished_at, exit_code, succeeded)
                VALUES ('legacy-job', 10, 20, 0, 1);
                ",
            )
            .expect("legacy schema");
        drop(legacy);

        let migrated = open(&path).expect("migrated database");
        let record = last_run(&migrated, "legacy-job")
            .expect("last run")
            .expect("legacy row");
        assert_eq!(record.started_at, 10_000);
        assert_eq!(record.finished_at, 20_000);

        let columns = migrated
            .prepare("PRAGMA table_info(runs)")
            .expect("table info")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("columns")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("column names");
        assert!(columns.iter().any(|column| column == "started_at_ms"));
        assert!(columns.iter().any(|column| column == "finished_at_ms"));
    }
}
