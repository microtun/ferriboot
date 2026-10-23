//! Error type mirroring picotool's error categories (and exit codes).

use std::fmt;

/// Error category, matching the `ERROR_*` codes used by picotool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Args,
    Format,
    Incompatible,
    ReadFailed,
    WriteFailed,
    NotPossible,
    VerificationFailed,
    Unknown,
}

impl ErrorKind {
    /// The process exit code picotool uses for this category.
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorKind::Args => -1,
            ErrorKind::Format => -2,
            ErrorKind::Incompatible => -3,
            ErrorKind::ReadFailed => -4,
            ErrorKind::WriteFailed => -5,
            ErrorKind::NotPossible => -8,
            ErrorKind::VerificationFailed => -11,
            ErrorKind::Unknown => -99,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

macro_rules! bail {
    ($kind:ident, $($arg:tt)*) => {
        return Err($crate::romseal::error::Error::new($crate::romseal::error::ErrorKind::$kind, format!($($arg)*)))
    };
}

macro_rules! err {
    ($kind:ident, $($arg:tt)*) => {
        $crate::romseal::error::Error::new($crate::romseal::error::ErrorKind::$kind, format!($($arg)*))
    };
}

pub(crate) use bail;
pub(crate) use err;
