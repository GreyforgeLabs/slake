use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use serde::Serialize;

use crate::guard;
use crate::model::{ClearResult, RunResult, StatusResult};

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
            Ok(result
                .exit_code
                .unwrap_or(if result.skipped { 0 } else { 1 }))
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

fn print_run_result(result: &RunResult, json: bool) -> Result<()> {
    if json {
        return print_json(result);
    }

    match result.action {
        "skip" => {
            println!(
                "name={} action=skip reason=cooldown last_exit_code={} remaining={}",
                result.name,
                result
                    .last_exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                result
                    .remaining_seconds
                    .map(guard::format_duration)
                    .unwrap_or_else(|| "0s".to_owned()),
            );
        }
        _ => {
            println!(
                "name={} action=run exit_code={} finished_at={}",
                result.name,
                result
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_owned()),
                result.finished_at.as_deref().unwrap_or("unknown"),
            );
        }
    }

    Ok(())
}

fn print_status_result(result: &StatusResult, json: bool) -> Result<()> {
    if json {
        return print_json(result);
    }

    match result.state.as_str() {
        "never-run" => {
            println!("name={} action=status state=never-run", result.name);
        }
        "cooling-down" => {
            println!(
                "name={} action=status state=cooling-down last_exit_code={} last_finished_at={} remaining={}",
                result.name,
                result
                    .last_exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                result.last_finished_at.as_deref().unwrap_or("unknown"),
                result
                    .remaining_seconds
                    .map(guard::format_duration)
                    .unwrap_or_else(|| "0s".to_owned()),
            );
        }
        _ => {
            println!(
                "name={} action=status state=ready last_exit_code={} last_finished_at={} elapsed={}",
                result.name,
                result
                    .last_exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                result.last_finished_at.as_deref().unwrap_or("unknown"),
                result
                    .elapsed_seconds
                    .map(guard::format_duration)
                    .unwrap_or_else(|| "0s".to_owned()),
            );
        }
    }

    Ok(())
}

fn print_clear_result(result: &ClearResult, json: bool) -> Result<()> {
    if json {
        return print_json(result);
    }

    println!(
        "name={} action=clear deleted_runs={}",
        result.name, result.deleted_runs
    );
    Ok(())
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
