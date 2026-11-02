//! Bounded, inspectable failures at the data and source boundaries.
use std::fmt;

/// A failure category suitable for a consumer status panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// JSON is malformed, incomplete, or contains unknown fields.
    InvalidJson,
    /// A schema or adapter version is not supported.
    UnsupportedVersion,
    /// A value violates the format.
    InvalidValue,
    /// An input exceeds its resource budget.
    Limit,
    /// A stable ID appears more than once in its namespace.
    DuplicateId,
    /// A model reference does not exist.
    MissingReference,
    /// An update belongs to another source or coordinate contract.
    SourceMismatch,
    /// An update has an older sequence.
    StaleRevision,
    /// The revision or expected base conflicts with the visible model.
    RevisionConflict,
    /// Reading a source failed.
    Io,
    /// A selection does not exist.
    InvalidSelection,
    /// The selected anchor is unavailable.
    NoAlignment,
}

/// A failure with a model path and a short explanation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    /// Machine-readable category.
    pub kind: ErrorKind,
    /// Field or source location.
    pub path: String,
    /// Human-readable explanation.
    pub message: String,
}

impl Error {
    /// Construct a failure without retaining unbounded source content.
    pub(crate) fn new(
        kind: ErrorKind,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            path: path.into().chars().take(512).collect(),
            message: message.into().chars().take(1024).collect(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} at {}: {}", self.kind, self.path, self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::new(ErrorKind::Io, "source", error.to_string())
    }
}
