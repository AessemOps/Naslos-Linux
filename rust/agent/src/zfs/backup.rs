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
        ZfsError::Other(m) => ZfsError::Other(format!("{prefix}{m}")),
    }
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
            ZfsError::Other(format!(
                "unexpected size {:?} from zfs send -nP: {e}",
                value
            ))
        })?;
        return Ok(size);
    }
    Err(ZfsError::Other("zfs send -nP did not report a size".into()))
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

    /// A dataset's snapshots with GUIDs, in creation order.
    pub async fn snapshots_with_guid(&self, dataset: &str) -> ZfsResult<Vec<SnapshotInfo>> {
        validate_dataset_path(dataset)?;
        let out = match self
            .host_exec(
                ZFS_BIN,
                &[
                    "list".to_string(),
                    "-H".into(),
                    "-p".into(),
                    "-t".into(),
                    "snapshot".into(),
                    "-o".into(),
                    "name,guid,creation".into(),
                    "-s".into(),
                    "creation".into(),
                    "-r".into(),
                    dataset.to_string(),
                ],
            )
            .await
        {
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
            let name = match fields[0].find('@') {
                Some(at) => &fields[0][at + 1..],
                None => fields[0],
            };
            snapshots.push(SnapshotInfo {
                name: name.to_string(),
                guid: fields[1].to_string(),
                created: fields.get(2).map(|s| s.to_string()).unwrap_or_default(),
            });
        }
        Ok(snapshots)
    }

    /// Start `zfs receive` and return its stdin plus a waiter.
    pub async fn receive_stream(
        &self,
        dataset: &str,
        force: bool,
    ) -> ZfsResult<(BoxedWrite, Waiter)> {
        validate_dataset_path(dataset)?;
        let mut argv = vec!["receive".to_string()];
        if force {
            argv.push("-F".into());
        }
        argv.push(dataset.to_string());
        self.runner_spawn_in(&argv).await
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
