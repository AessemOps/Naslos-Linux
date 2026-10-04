//! Naslos privileged agent (Rust port of the Go `agent/` module).
//!
//! The agent runs as a privileged, host-networked DaemonSet and is the only
//! component that executes `zpool`/`zfs`/`wipefs` (via `chroot /host`) and
//! writes the rendered share configuration onto the host. The HTTP contract is
//! the same as the Go agent's.

pub mod server;
pub mod shares;
pub mod zfs;
