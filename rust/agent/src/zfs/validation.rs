//! Error classification for the HTTP layer: caller-fixable input is a
//! `Validation` (400), anything else a node-side failure (`Other`, 500).

/// A ZFS operation error, classified the way the Go `ValidationError` was.
#[derive(Debug, thiserror::Error)]
pub enum ZfsError {
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    Other(String),
}

impl ZfsError {
    pub fn is_validation(&self) -> bool {
        matches!(self, ZfsError::Validation(_))
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
