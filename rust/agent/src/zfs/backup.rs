//! Streaming backup: `zfs send`/`receive` as pipes (port of `zfs/backup.go`).

use super::dataset::validate_dataset_path;
use super::runner::{BoxedRead, BoxedWrite, Waiter};
use super::types::{SendStreamOptions, SnapshotInfo};
use super::{ZfsClient, ZFS_BIN};
use crate::invalid;
use crate::zfs::validation::{ZfsError, ZfsResult};

/// What a snapshot component may look like.
fn is_valid_snapshot_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '+' | '-'))
}

/// Reject anything that could turn a snapshot argument into a flag or a second
/// argument.
pub fn validate_snapshot_name(name: &str) -> ZfsResult<()> {
    if name.trim().is_empty() {
        return Err(ZfsError::Validation("snapshot name is required".into()));
    }
    if name.starts_with('-') {
        return Err(invalid!("snapshot name {:?} must not start with '-'", name));
    }
    if name.contains('@') || name.contains(' ') || name.contains('/') || name.contains('\\') {
        return Err(invalid!(
            "snapshot name {:?} must not contain '@', '/', '\\' or spaces",
            name
        ));
    }
    if !is_valid_snapshot_name(name) {
        return Err(invalid!(
            "snapshot name {:?} must be letters, digits, '.', '_', ':', '+' or '-'",
            name
        ));
    }
    Ok(())
}

/// Preserve the error classification when adding context (the Go `%w` wrap
/// still let `errors.As` find the `ValidationError`).
fn wrap(prefix: &str, err: ZfsError) -> ZfsError {
    match err {
        ZfsError::Validation(m) => ZfsError::Validation(format!("{prefix}{m}")),
        ZfsError::Conflict(m) => ZfsError::Conflict(format!("{prefix}{m}")),
        ZfsError::Other(m) => ZfsError::Other(format!("{prefix}{m}")),
    }
}

/// The `zfs list` argv for a dataset's snapshots. `recursive` includes child
/// datasets; replication passes `false` so a child GUID cannot be mistaken for
/// the root's. The caller validates `dataset`.
pub fn snapshot_list_argv(dataset: &str, recursive: bool) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "list".into(),
        "-H".into(),
        "-p".into(),
        "-t".into(),
        "snapshot".into(),
        "-o".into(),
        "name,guid,creation".into(),
        "-s".into(),
        "creation".into(),
    ];
    if recursive {
        argv.push("-r".into());
    }
    argv.push(dataset.to_string());
    argv
}

/// Build the argument list for `zfs send`; the single place deciding what a
/// backup contains.
pub fn send_command_line(opts: &SendStreamOptions) -> ZfsResult<Vec<String>> {
    validate_dataset_path(&opts.dataset)?;
    validate_snapshot_name(&opts.to).map_err(|e| wrap("to: ", e))?;

    let mut argv = vec!["send".to_string()];
    if opts.raw {
        argv.push("-w".into());
    }
    if opts.recursive {
        // Lowercase `-r`: replicate descendants without carrying properties
        // such as the source's mountpoint, which would shadow the live dataset
        // on the same node.
        argv.push("-r".into());
    }
    if !opts.from.is_empty() {
        validate_snapshot_name(&opts.from).map_err(|e| wrap("from: ", e))?;
        if opts.from == opts.to {
            return Err(invalid!(
                "base and target are the same snapshot ({}): an incremental send needs two",
                opts.to
            ));
        }
        argv.push("-i".into());
        argv.push(format!("{}@{}", opts.dataset, opts.from));
    }
    argv.push(format!("{}@{}", opts.dataset, opts.to));
    Ok(argv)
}

/// Read the `size\t<bytes>` line of a parsable dry run.
pub fn parse_send_size(out: &str) -> ZfsResult<i64> {
    for line in out.split('\n') {
        let line = line.trim();
        let mut fields = line.splitn(2, '\t');
        let key = fields.next().unwrap_or("");
        let Some(value) = fields.next() else { continue };
        if key != "size" {
            continue;
        }
        let size: i64 = value.trim().parse().map_err(|e| {
            ZfsError::Other(format!("unexpected size {value:?} from zfs send -nP: {e}"))
        })?;
        return Ok(size);
    }
    Err(ZfsError::Other("zfs send -nP did not report a size".into()))
}

/// Snapshot prefixes owned by Naslos features. A destination snapshot outside
/// this set is a user snapshot and a forced receive must not delete it.
pub const OWNED_SNAPSHOT_PREFIXES: [&str; 3] = ["naslos-rep-", "naslos-", "buddy-"];

/// The `zfs receive` argv. `-u` pauses mounting and `-x` drops the stream's
/// host layout so a peer cannot choose where the dataset lands.
///
/// The excluded-property policy is deliberate and narrow:
/// - `mountpoint` and `canmount` would let a peer choose any host path (or
///   hide a dataset from the deliberate mount pass),
/// - `sharenfs`/`sharesmb` would make the received dataset an export without
///   the share store or the export-canary checks ever seeing it.
///
/// Everything else (compression, quota, recordsize, …) is kept from the
/// stream: it describes the data, not this node's layout. `finalize_receive`
/// then sets `/var/mnt/<pool>/…` and mounts the tree explicitly.
pub fn receive_command_line(dataset: &str, force: bool) -> Vec<String> {
    let mut argv: Vec<String> = vec!["receive".into(), "-u".into()];
    for prop in ["mountpoint", "canmount", "sharenfs", "sharesmb"] {
        argv.push("-x".into());
        argv.push(prop.into());
    }
    if force {
        argv.push("-F".into());
    }
    argv.push(dataset.to_string());
    argv
}

/// The server-chosen mountpoint for a received dataset:
/// `/var/mnt/<pool>/<relative path>`. `None` when the path is not
/// `<pool>/<dataset>`.
pub fn receive_mountpoint(dataset: &str) -> Option<String> {
    let (pool, rest) = dataset.split_once('/')?;
    if pool.is_empty() || rest.is_empty() {
        return None;
    }
    Some(format!("/var/mnt/{pool}/{rest}"))
}

/// The first destination snapshot whose name is not a Naslos-owned prefix.
pub fn foreign_snapshot(listing: &str) -> Option<String> {
    for line in listing.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some((_, snap)) = line.split_once('@') else {
            continue;
        };
        if !OWNED_SNAPSHOT_PREFIXES
            .iter()
            .any(|prefix| snap.starts_with(prefix))
        {
            return Some(snap.to_string());
        }
    }
    None
}

/// What [`ZfsClient::finalize_receive`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReceiveFinalize {
    pub mountpoint: String,
    pub warnings: Vec<String>,
}

impl ZfsClient {
    /// Start a `zfs send` and return its stdout plus a waiter.
    pub async fn send_stream(&self, opts: &SendStreamOptions) -> ZfsResult<(BoxedRead, Waiter)> {
        let argv = send_command_line(opts)?;
        self.runner_spawn_out(&argv).await
    }

    /// Ask ZFS how large the stream would be (`zfs send -n -P`).
    pub async fn estimate_send(&self, opts: &SendStreamOptions) -> ZfsResult<i64> {
        let argv = send_command_line(opts)?;
        let mut with_dry_run = vec!["send".to_string(), "-n".into(), "-P".into()];
        with_dry_run.extend(argv[1..].iter().cloned());

        match self.host_exec(ZFS_BIN, &with_dry_run).await {
            Ok(out) => parse_send_size(&out),
            Err(e) => Err(ZfsError::Other(format!(
                "estimating send size: {}: {}",
                e.output.trim(),
                e.message
            ))),
        }
    }

    /// A dataset's snapshots with GUIDs, in creation order. `recursive` also
    /// lists child-dataset snapshots; replication base selection asks for the
    /// shallow list so a child snapshot GUID cannot be mistaken for the root's.
    pub async fn snapshots_with_guid(
        &self,
        dataset: &str,
        recursive: bool,
    ) -> ZfsResult<Vec<SnapshotInfo>> {
        validate_dataset_path(dataset)?;
        let argv = snapshot_list_argv(dataset, recursive);
        let out = match self.host_exec(ZFS_BIN, &argv).await {
            Ok(out) => out,
            Err(e) => return Err(ZfsError::Other(format!("listing snapshots: {}", e.message))),
        };

        let mut snapshots = Vec::new();
        for line in out.trim().split('\n') {
            if line.is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 2 {
                continue;
            }
            let (dataset, name) = match fields[0].find('@') {
                Some(at) => (&fields[0][..at], &fields[0][at + 1..]),
                None => (fields[0], fields[0]),
            };
            snapshots.push(SnapshotInfo {
                dataset: dataset.to_string(),
                name: name.to_string(),
                guid: fields[1].to_string(),
                created: fields.get(2).map(|s| s.to_string()).unwrap_or_default(),
            });
        }
        Ok(snapshots)
    }

    /// Start `zfs receive` and return its stdin plus a waiter.
    ///
    /// The stream must not choose the host mount layout: `-u` defers mounting
    /// and `-x` drops the stream's `mountpoint`, `canmount`, `sharenfs` and
    /// `sharesmb`. The API sets a server-chosen mountpoint after a successful
    /// receive ([`Self::finalize_receive`]).
    ///
    /// `-F` is only accepted after the destination passed
    /// [`Self::receive_force_gate`]: a stream may replace this feature's own
    /// snapshots, never a user snapshot or a foreign child dataset.
    pub async fn receive_stream(
        &self,
        dataset: &str,
        force: bool,
    ) -> ZfsResult<(BoxedWrite, Waiter)> {
        validate_dataset_path(dataset)?;
        if force {
            self.receive_force_gate(dataset).await?;
        }
        let argv = receive_command_line(dataset, force);
        self.runner_spawn_in(&argv).await
    }

    /// Verify a forced receive cannot destroy anything this feature did not
    /// create: every existing snapshot must carry a Naslos-owned prefix and
    /// the destination must have no child datasets.
    pub async fn receive_force_gate(&self, dataset: &str) -> ZfsResult<()> {
        let snapshots = match self
            .host_exec(
                ZFS_BIN,
                &[
                    "list".to_string(),
                    "-H".to_string(),
                    "-o".to_string(),
                    "name".to_string(),
                    "-t".to_string(),
                    "snapshot".to_string(),
                    "-r".to_string(),
                    dataset.to_string(),
                ],
            )
            .await
        {
            Ok(out) => out,
            Err(e) => {
                if e.output.to_ascii_lowercase().contains("does not exist") {
                    return Ok(());
                }
                return Err(ZfsError::Other(format!(
                    "listing destination snapshots: {}: {}",
                    e.output.trim(),
                    e.message
                )));
            }
        };
        if let Some(foreign) = foreign_snapshot(&snapshots) {
            return Err(crate::conflict!(
                "destination {dataset} holds snapshot {foreign:?} that Naslos did not create; refusing a forced receive. Remove that snapshot or receive into another dataset"
            ));
        }

        let children = match self
            .host_exec(
                ZFS_BIN,
                &[
                    "list".to_string(),
                    "-H".to_string(),
                    "-o".to_string(),
                    "name".to_string(),
                    "-r".to_string(),
                    dataset.to_string(),
                ],
            )
            .await
        {
            Ok(out) => out,
            Err(e) => {
                if e.output.to_ascii_lowercase().contains("does not exist") {
                    return Ok(());
                }
                return Err(ZfsError::Other(format!(
                    "listing destination children: {}: {e}",
                    e.output.trim()
                )));
            }
        };
        for line in children.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if line != dataset {
                return Err(crate::conflict!(
                    "destination {dataset} has child dataset {line}; a forced receive could replace it. Destroy the replica dataset first"
                ));
            }
        }
        Ok(())
    }

    /// Set the server-chosen mountpoint after a successful receive and mount
    /// the received tree deliberately.
    ///
    /// A recursive receive creates the children but `-u` leaves every dataset
    /// unmounted, so only mounting the root would send child writes into
    /// shadow directories under the root's mountpoint. The tree is enumerated
    /// with a scoped `zfs list -r <dataset>`; `zfs mount -a` is never used
    /// because it would also mount unrelated datasets.
    ///
    /// A mount failure is reported as a warning: the received data is already
    /// durable and must not be reported as lost. Failing to set the root
    /// mountpoint is still an error (there is no usable layout at all).
    pub async fn finalize_receive(&self, dataset: &str) -> ZfsResult<ReceiveFinalize> {
        validate_dataset_path(dataset)?;
        let mountpoint = receive_mountpoint(dataset).ok_or_else(|| {
            invalid!("cannot choose a mountpoint for {dataset}: expected /var/mnt/<pool>/<dataset>")
        })?;
        let prop = format!("mountpoint={mountpoint}");
        if let Err(e) = self
            .host_exec(ZFS_BIN, &["set".to_string(), prop, dataset.to_string()])
            .await
        {
            return Err(ZfsError::Other(format!(
                "setting the receive mountpoint to {mountpoint}: {}: {}",
                e.output.trim(),
                e.message
            )));
        }

        let mut warnings = Vec::new();
        let listing = self
            .host_exec(
                ZFS_BIN,
                &[
                    "list".to_string(),
                    "-H".to_string(),
                    "-o".to_string(),
                    "name,mountpoint,canmount".to_string(),
                    "-r".to_string(),
                    dataset.to_string(),
                ],
            )
            .await;
        let rows = match listing {
            Ok(out) => out,
            Err(e) => {
                warnings.push(format!(
                    "received {dataset} but could not list its children, so only the root was mounted: {}",
                    e.output.trim()
                ));
                // Still try the root; the data is durable either way.
                self.mount_received(dataset, &mountpoint, &mut warnings)
                    .await;
                return Ok(ReceiveFinalize {
                    mountpoint,
                    warnings,
                });
            }
        };

        // `zfs list -r` prints parents before children, so each mountpoint
        // exists by the time its dataset is mounted.
        for line in rows.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 3 {
                continue;
            }
            let (child, current, canmount) = (fields[0], fields[1], fields[2]);
            let Some(expected) = receive_mountpoint(child) else {
                warnings.push(format!(
                    "received {child} but it is outside {dataset}; left it unmounted"
                ));
                continue;
            };
            if child == dataset {
                if current != expected {
                    // The root mountpoint was set just above; a stale value
                    // means `zfs set` did not take effect, which is worth a
                    // warning instead of mounting onto the wrong path.
                    warnings.push(format!(
                        "received {child} but its mountpoint is {current:?} instead of {expected:?}; not mounted"
                    ));
                    continue;
                }
            } else if current != expected {
                let prop = format!("mountpoint={expected}");
                if let Err(e) = self
                    .host_exec(ZFS_BIN, &["set".to_string(), prop, child.to_string()])
                    .await
                {
                    warnings.push(format!(
                        "received {child} but could not set its mountpoint to {expected}: {}",
                        e.output.trim()
                    ));
                    continue;
                }
            }
            if canmount == "off" {
                continue;
            }
            self.mount_received(child, &expected, &mut warnings).await;
        }
        Ok(ReceiveFinalize {
            mountpoint,
            warnings,
        })
    }

    async fn mount_received(&self, dataset: &str, mountpoint: &str, warnings: &mut Vec<String>) {
        match self
            .host_exec(ZFS_BIN, &["mount".to_string(), dataset.to_string()])
            .await
        {
            Ok(_) => {}
            Err(e) => {
                let lower = e.output.to_ascii_lowercase();
                if !lower.contains("already mounted") {
                    warnings.push(format!(
                        "received {dataset} but could not mount it at {mountpoint}: {}",
                        e.output.trim()
                    ));
                }
            }
        }
    }

    /// Discard the partial state of a failed receive.
    ///
    /// `zfs receive -A` aborts an interrupted receive: it clears the resume
    /// token, destroys `%recv` snapshots and rolls a partially received
    /// incremental back to its pre-receive state (or destroys a dataset a
    /// failed full receive created). This is best effort — the caller has
    /// already failed and must not mask the original error.
    pub async fn abort_receive(&self, dataset: &str) {
        if validate_dataset_path(dataset).is_err() {
            return;
        }
        match self
            .host_exec(
                ZFS_BIN,
                &["receive".to_string(), "-A".to_string(), dataset.to_string()],
            )
            .await
        {
            Ok(_) => tracing::info!("discarded the partial receive of {dataset}"),
            Err(e) => tracing::warn!(
                "could not discard the partial receive of {dataset}: {}: {}",
                e.output.trim(),
                e.message
            ),
        }
    }

    async fn runner_spawn_out(&self, argv: &[String]) -> ZfsResult<(BoxedRead, Waiter)> {
        self.runner
            .spawn_out(ZFS_BIN, argv)
            .await
            .map_err(|e| ZfsError::Validation(e.to_string()))
    }

    async fn runner_spawn_in(&self, argv: &[String]) -> ZfsResult<(BoxedWrite, Waiter)> {
        self.runner
            .spawn_in(ZFS_BIN, argv)
            .await
            .map_err(|e| ZfsError::Validation(e.to_string()))
    }
}
