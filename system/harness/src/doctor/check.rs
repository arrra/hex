use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Category {
    Health,
    Config,
    Security,
    Performance,
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Category::Health => write!(f, "Health"),
            Category::Config => write!(f, "Config"),
            Category::Security => write!(f, "Security"),
            Category::Performance => write!(f, "Performance"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Pass,
    Warn,
    Fail,
    /// Auto-repaired — counts as pass in summary
    Fixed,
    /// Informational only — does NOT increment warn/fail counters
    Info,
    /// Skipped because a prerequisite was not met
    Skip,
}

impl Status {
    pub fn is_error(&self) -> bool {
        matches!(self, Status::Fail)
    }
    pub fn is_warning(&self) -> bool {
        matches!(self, Status::Warn)
    }
    pub fn counts_as_pass(&self) -> bool {
        matches!(self, Status::Pass | Status::Fixed | Status::Skip | Status::Info)
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Pass => write!(f, "PASS"),
            Status::Warn => write!(f, "WARN"),
            Status::Fail => write!(f, "FAIL"),
            Status::Fixed => write!(f, "FIXED"),
            Status::Info => write!(f, "INFO"),
            Status::Skip => write!(f, "SKIP"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub status: Status,
    pub message: String,
    pub details: Option<String>,
    pub elapsed_ms: u64,
}

impl CheckResult {
    pub fn pass(msg: impl Into<String>) -> Self {
        Self { status: Status::Pass, message: msg.into(), details: None, elapsed_ms: 0 }
    }
    pub fn warn(msg: impl Into<String>) -> Self {
        Self { status: Status::Warn, message: msg.into(), details: None, elapsed_ms: 0 }
    }
    pub fn fail(msg: impl Into<String>) -> Self {
        Self { status: Status::Fail, message: msg.into(), details: None, elapsed_ms: 0 }
    }
    pub fn skip(msg: impl Into<String>) -> Self {
        Self { status: Status::Skip, message: msg.into(), details: None, elapsed_ms: 0 }
    }
    pub fn fixed(msg: impl Into<String>) -> Self {
        Self { status: Status::Fixed, message: msg.into(), details: None, elapsed_ms: 0 }
    }
    pub fn info(msg: impl Into<String>) -> Self {
        Self { status: Status::Info, message: msg.into(), details: None, elapsed_ms: 0 }
    }
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }
}

/// Runtime context passed to every check.
#[derive(Debug, Clone)]
pub struct Context {
    pub hex_dir: PathBuf,
    pub home: PathBuf,
    pub fix: bool,
}

impl Context {
    pub fn new(hex_dir: PathBuf, fix: bool) -> Self {
        let home = dirs_home();
        Self { hex_dir, home, fix }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

/// Core trait — implement one per health check.
/// All impls must be Send + Sync so the runner can parallelize them.
pub trait DoctorCheck: Send + Sync {
    /// Short stable identifier (e.g. "hex-dir-set"). Used for --filter and JSON.
    fn name(&self) -> &str;

    /// Human-readable display name (defaults to name()).
    fn display_name(&self) -> &str {
        self.name()
    }

    /// Functional category for grouping.
    fn category(&self) -> Category;

    /// Run the check. Must NOT panic.
    fn run(&self, ctx: &Context) -> CheckResult;
}
