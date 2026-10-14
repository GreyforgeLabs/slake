use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use crate::guard;
use crate::model::{ClearResult, GuardState, RunResult, StatusResult};

#[derive(Parser, Debug)]
#[command(
    name = "slake",
    version,
    about = "Minimum-interval guard for cron jobs and recurring commands."
)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,

    #[arg(long, global = true, value_name = "PATH")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Run(RunArgs),
    Status(StatusArgs),
    Clear(ClearArgs),
}

#[derive(Args, Debug)]
struct StatusArgs {
    #[arg(long)]
    name: String,

    #[arg(long, value_name = "DURATION")]
    min_interval: String,

    #[arg(long, value_name = "DURATION")]
    failure_backoff: Option<String>,
}

#[derive(Args, Debug)]
struct ClearArgs {
    #[arg(long)]
    name: String,

    #[arg(
        long,
        help = "Clear an active claim too; a running child may overlap a new run"
    )]
    force: bool,
}

#[derive(Args, Debug)]
#[command(trailing_var_arg = true)]
struct RunArgs {
    #[arg(long)]
    name: String,

    #[arg(long, value_name = "DURATION")]
    min_interval: String,

    #[arg(long, value_name = "DURATION")]
    failure_backoff: Option<String>,

    #[arg(long, value_name = "DURATION", default_value = "24h")]
    lease: String,

    #[arg(required = true, value_name = "COMMAND")]
    command: Vec<String>,
}

pub fn run() -> Result<i32> {
    let cli = Cli::parse();
    let db_path = match cli.db {
        Some(path) => path,
        None => guard::default_db_path()?,
    };
    let mut connection = guard::open_database(&db_path)?;

    match cli.command {
        Commands::Run(args) => {
            let min_interval = guard::parse_min_interval(&args.min_interval)?;
            let failure_backoff = args
                .failure_backoff
                .as_deref()
                .map(guard::parse_failure_backoff)
                .transpose()?
                .unwrap_or(min_interval);
            let lease_duration = guard::parse_lease_duration(&args.lease)?;
            let result = guard::run_guarded(
                &mut connection,
                &args.name,
                min_interval,
                failure_backoff,
                lease_duration,
                &args.command,
            )?;
            print_run_result(&result, cli.json)?;
            if let Some(signal) = result.signal {
                guard::end_like_command(signal);
            }
            Ok(exit_code(&result))
        }
        Commands::Status(args) => {
            let min_interval = guard::parse_min_interval(&args.min_interval)?;
            let failure_backoff = args
                .failure_backoff
                .as_deref()
                .map(guard::parse_failure_backoff)
                .transpose()?
                .unwrap_or(min_interval);
            let result = guard::status(&connection, &args.name, min_interval, failure_backoff)?;
            print_status_result(&result, cli.json)?;
            Ok(0)
        }
        Commands::Clear(args) => {
            let result = guard::clear(&mut connection, &args.name, args.force)?;
            print_clear_result(&result, cli.json)?;
            Ok(0)
        }
    }
}

/// The process exit code for a run: the child's own code, `128 + signal` when a
/// signal terminated it (the shell convention), and 0 for a cooldown skip.
/// SIGHUP, SIGINT and SIGTERM are re-raised first, so for those the shell sees
/// the same `128 + signal` and other parents see a signal death.
fn exit_code(result: &RunResult) -> i32 {
    match (result.exit_code, result.signal) {
        (Some(code), _) => code,
        (None, Some(signal)) => 128 + signal,
        (None, None) => i32::from(!result.skipped),
    }
}

fn print_run_result(result: &RunResult, json: bool) -> Result<()> {
    if json {
        return print_json(&[
            ("name", Json::Str(&result.name)),
            ("action", Json::Str(result.action)),
            ("skipped", Json::Bool(result.skipped)),
            ("exit_code", result.exit_code.into()),
            ("last_exit_code", result.last_exit_code.into()),
            ("started_at", result.started_at.as_deref().into()),
            ("finished_at", result.finished_at.as_deref().into()),
            ("remaining_seconds", result.remaining_seconds.into()),
            ("signal", result.signal.into()),
        ]);
    }

    let line = if result.skipped {
        format!(
            "name={} action=skip reason=cooldown last_exit_code={} remaining={}",
            result.name,
            or_text(result.last_exit_code, "unknown"),
            remaining(result.remaining_seconds),
        )
    } else {
        let mut line = format!(
            "name={} action=run exit_code={} finished_at={}",
            result.name,
            or_text(result.exit_code, "signal"),
            result.finished_at.as_deref().unwrap_or("unknown"),
        );
        if let Some(signal) = result.signal {
            let _ = write!(line, " signal={signal}");
        }
        line
    };
    print_line(&line)
}

fn print_status_result(result: &StatusResult, json: bool) -> Result<()> {
    if json {
        return print_json(&[
            ("name", Json::Str(&result.name)),
            ("state", Json::Str(result.state.as_str())),
            ("last_exit_code", result.last_exit_code.into()),
            ("last_succeeded", result.last_succeeded.into()),
            ("last_started_at", result.last_started_at.as_deref().into()),
            (
                "last_finished_at",
                result.last_finished_at.as_deref().into(),
            ),
            ("elapsed_seconds", result.elapsed_seconds.into()),
            ("remaining_seconds", result.remaining_seconds.into()),
        ]);
    }

    let mut line = format!(
        "name={} action=status state={}",
        result.name,
        result.state.as_str()
    );
    match result.state {
        GuardState::NeverRun => {}
        state => {
            let (label, seconds) = if state == GuardState::CoolingDown {
                ("remaining", result.remaining_seconds)
            } else {
                ("elapsed", result.elapsed_seconds)
            };
            let _ = write!(
                line,
                " last_exit_code={} last_finished_at={} {label}={}",
                or_text(result.last_exit_code, "unknown"),
                result.last_finished_at.as_deref().unwrap_or("unknown"),
                remaining(seconds),
            );
        }
    }
    print_line(&line)
}

fn print_clear_result(result: &ClearResult, json: bool) -> Result<()> {
    if json {
        return print_json(&[
            ("name", Json::Str(&result.name)),
            ("deleted_runs", Json::Int(result.deleted_runs as i64)),
        ]);
    }
    print_line(&format!(
        "name={} action=clear deleted_runs={}",
        result.name, result.deleted_runs
    ))
}

fn or_text(code: Option<i32>, fallback: &str) -> String {
    code.map_or_else(|| fallback.to_owned(), |code| code.to_string())
}

fn remaining(seconds: Option<u64>) -> String {
    seconds.map_or_else(|| "0s".to_owned(), guard::format_duration)
}

/// Writes one result line to stdout. A closed or failing stdout is reported as
/// a runtime error instead of aborting the process.
fn print_line(line: &str) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")
        .and_then(|()| stdout.flush())
        .context("failed to write result to stdout")
}

/// A JSON scalar. Results are flat objects, so this is the whole data model.
enum Json<'a> {
    Str(&'a str),
    Int(i64),
    Bool(bool),
    Null,
}

impl<'a> From<Option<&'a str>> for Json<'a> {
    fn from(value: Option<&'a str>) -> Self {
        value.map_or(Json::Null, Json::Str)
    }
}

impl From<Option<i32>> for Json<'_> {
    fn from(value: Option<i32>) -> Self {
        value.map_or(Json::Null, |value| Json::Int(value.into()))
    }
}

impl From<Option<u64>> for Json<'_> {
    fn from(value: Option<u64>) -> Self {
        // Durations are capped at i64::MAX milliseconds, so seconds always fit.
        value.map_or(Json::Null, |value| Json::Int(value as i64))
    }
}

impl From<Option<bool>> for Json<'_> {
    fn from(value: Option<bool>) -> Self {
        value.map_or(Json::Null, Json::Bool)
    }
}

/// Prints a flat object in the same layout as `serde_json::to_string_pretty`.
fn print_json(fields: &[(&str, Json)]) -> Result<()> {
    let mut out = String::from("{");
    for (index, (key, value)) in fields.iter().enumerate() {
        out.push_str(if index == 0 { "\n  " } else { ",\n  " });
        push_json_string(&mut out, key);
        out.push_str(": ");
        match value {
            Json::Str(text) => push_json_string(&mut out, text),
            Json::Int(number) => {
                let _ = write!(out, "{number}");
            }
            Json::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
            Json::Null => out.push_str("null"),
        }
    }
    out.push_str("\n}");
    print_line(&out)
}

fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            control if control < ' ' => {
                let _ = write!(out, "\\u{:04x}", u32::from(control));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::{Json, push_json_string};

    #[test]
    fn json_strings_are_escaped_like_serde_json() {
        let mut out = String::new();
        push_json_string(&mut out, "a\"b\\c\nd\u{01}e");
        assert_eq!(out, r#""a\"b\\c\nd\u0001e""#);
    }

    #[test]
    fn json_options_map_to_null() {
        assert!(matches!(Json::from(None::<i32>), Json::Null));
        assert!(matches!(Json::from(Some(3_u64)), Json::Int(3)));
    }
}
