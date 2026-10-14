#[derive(Debug, Clone)]
pub struct RunRecord {
    pub name: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub exit_code: Option<i32>,
    pub succeeded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardState {
    NeverRun,
    Ready,
    CoolingDown,
}

impl GuardState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeverRun => "never-run",
            Self::Ready => "ready",
            Self::CoolingDown => "cooling-down",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StatusResult {
    pub name: String,
    pub state: GuardState,
    pub last_exit_code: Option<i32>,
    pub last_succeeded: Option<bool>,
    pub last_started_at: Option<String>,
    pub last_finished_at: Option<String>,
    pub elapsed_seconds: Option<u64>,
    pub remaining_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub name: String,
    pub action: &'static str,
    pub skipped: bool,
    pub exit_code: Option<i32>,
    /// Signal that terminated the command, when it did not exit normally.
    pub signal: Option<i32>,
    pub last_exit_code: Option<i32>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub remaining_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ClearResult {
    pub name: String,
    pub deleted_runs: usize,
}
