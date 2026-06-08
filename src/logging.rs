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
