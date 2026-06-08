use std::cell::RefCell;

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
