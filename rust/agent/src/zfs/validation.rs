//! Error classification for the HTTP layer: caller-fixable input is a
//! `Validation` (400), anything else a node-side failure (`Other`, 500).

/// A ZFS operation error, classified the way the Go `ValidationError` was.
///
/// `Conflict` names a live consumer (a share, a VM, an app mount, a
/// replication job or a busy zvol/dataset) and is mapped to HTTP 409 so the
/// caller can tell "stop that consumer first" from a bad request.
#[derive(Debug, thiserror::Error)]
pub enum ZfsError {
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Other(String),
}

impl ZfsError {
    pub fn is_validation(&self) -> bool {
        matches!(self, ZfsError::Validation(_))
    }

    pub fn is_conflict(&self) -> bool {
        matches!(self, ZfsError::Conflict(_))
    }
}

pub type ZfsResult<T> = Result<T, ZfsError>;

/// `invalidf` equivalent.
#[macro_export]
macro_rules! invalid {
    ($($arg:tt)*) => {
        $crate::zfs::validation::ZfsError::Validation(format!($($arg)*))
    };
}

/// `conflictf` equivalent: a live consumer refuses the mutation with 409.
#[macro_export]
macro_rules! conflict {
    ($($arg:tt)*) => {
        $crate::zfs::validation::ZfsError::Conflict(format!($($arg)*))
    };
}
