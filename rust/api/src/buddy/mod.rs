//! Buddy Backup protocol primitives (port of `api/internal/buddy`): the
//! identity keys, the sealed-chunk envelope and the signed manifest.

pub const PATH_PREFIX: &str = "/api/buddy/v1";

pub mod auth;
pub mod client;
pub mod envelope;
pub mod keys;
pub mod peers;
pub mod store;

pub use auth::{Authenticator, RequestAuth};
pub use envelope::{Manifest, ManifestChunk};
pub use keys::{new_identity, Identity};
pub use peers::{Peer, PeerStore};
pub use store::{Backups, Store, StoreError};
