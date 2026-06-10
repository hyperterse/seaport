use std::cell::RefCell;
use std::env;
use std::sync::OnceLock;
use std::time::Duration;

/// Wall-clock phase timing for performance debugging, enabled with
/// `SEAPORT_TIMINGS=1`. Lines go to stderr so they interleave with, but do not
/// corrupt, normal progress output.
pub(crate) fn timings_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();

    *ENABLED.get_or_init(|| {
        env::var_os("SEAPORT_TIMINGS").is_some_and(|value| !value.is_empty() && value != "0")
    })
}

pub(crate) fn log_timing(task: &str, phase: &str, detail: &str, elapsed: Duration) {
    if !timings_enabled() {
        return;
    }

    eprintln!(
        "seaport-timing: {:>9.3}s  {:<12} {:<44} {detail}",
        elapsed.as_secs_f64(),
        phase,
        task
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum LogMode {
    Concise = 0,
    Verbose = 1,
    Quiet = 2,
}

impl LogMode {
    pub(crate) fn from_u8(value: u8) -> Self {
        match value {
            value if value == Self::Verbose as u8 => Self::Verbose,
            value if value == Self::Quiet as u8 => Self::Quiet,
            _ => Self::Concise,
        }
    }

    pub(crate) fn prints_events(self) -> bool {
        !matches!(self, Self::Quiet)
    }

    pub(crate) fn is_verbose(self) -> bool {
        matches!(self, Self::Verbose)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProgressLine {
    Banner(String),
    Step(String),
}

thread_local! {
    static PROGRESS_BUFFER: RefCell<Option<Vec<ProgressLine>>> = const { RefCell::new(None) };
}

pub(crate) fn begin_progress_buffer() {
    PROGRESS_BUFFER.with(|buffer| {
        *buffer.borrow_mut() = Some(Vec::new());
    });
}

pub(crate) fn push_progress_line(line: ProgressLine) -> bool {
    PROGRESS_BUFFER.with(|buffer| {
        let mut buffer = buffer.borrow_mut();

        let Some(lines) = buffer.as_mut() else {
            return false;
        };

        lines.push(line);
        true
    })
}

pub(crate) fn take_progress_buffer() -> Vec<ProgressLine> {
    PROGRESS_BUFFER.with(|buffer| buffer.borrow_mut().take().unwrap_or_default())
}
