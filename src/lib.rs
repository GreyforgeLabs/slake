//! slake keeps a SQLite ledger of command runs and enforces minimum intervals.
//!
//! The library target exists only so that the `slake` binary and the deprecated
//! `cooldown-guard` alias share one implementation. It is not a stable API.

mod cli;
mod db;
mod guard;
mod model;

/// One-line note the deprecated `cooldown-guard` alias prints to stderr before
/// behaving exactly like `slake`.
#[doc(hidden)]
pub const DEPRECATED_ALIAS_NOTE: &str = "cooldown-guard: deprecated alias; this tool is now `slake` (same arguments, same ledger). The alias will be removed in the next release.";

/// Parses the command line, runs the requested command and exits the process.
#[doc(hidden)]
pub fn main_entry() -> ! {
    match cli::run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::exit(2);
        }
    }
}
