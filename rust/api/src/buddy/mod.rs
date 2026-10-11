//! Buddy Backup protocol primitives (port of `api/internal/buddy`): the
//! identity keys, the sealed-chunk envelope and the signed manifest.

pub mod envelope;
pub mod keys;

pub use envelope::{Manifest, ManifestChunk};
pub use keys::{new_identity, Identity};
