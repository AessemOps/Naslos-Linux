//! Buddy backup tools (FR-MCP). Sends are long-running jobs (poll with
//! buddy_job_get); restores overwrite local datasets and confirm on the
//! dataset name.

use super::{
    b, confirm_field, n, no_args, s, s_enum, schema, Confirm, Method, Scope, Special, ToolDef,
};

pub const TOOLS: &[ToolDef] = &[
    tool!(
        "buddy_status",
        "The Buddy receiver status: enabled, storage, peers, quota.",
        Read,
        GET,
        "/api/buddy/status",
        no_args
    ),
    tool!(
        "buddy_peer_list",
        "The enrolled backup peers.",
        Read,
        GET,
        "/api/buddy/peers",
        no_args
    ),
    tool!(
        "buddy_identity_get",
        "This instance's backup identity (public key) and its receivers.",
        Read,
        GET,
        "/api/buddy/identity",
        no_args
    ),
    tool!(
        "buddy_job_list",
        "The recent backup send jobs.",
        Read,
        GET,
        "/api/buddy/jobs",
        no_args
    ),
    tool!(
        "buddy_job_get",
        "One backup job's progress and state.",
        Read,
        GET,
        "/api/buddy/jobs/{id}",
        || schema(&["id"], &[("id", s("The job id."))])
    ),
    tool!(
        "buddy_schedule_list",
        "The scheduled backup definitions.",
        Read,
        GET,
        "/api/buddy/schedules",
        no_args
    ),
    tool!(
        "buddy_send",
        "Start a backup send of one dataset to a peer. Long-running: poll buddy_job_get.",
        Write,
        POST,
        "/api/buddy/send",
        || schema(
            &["dataset", "source", "receiver"],
            &[
                ("dataset", s("The dataset to back up (pool/name).")),
                (
                    "source",
                    s("The name this instance presents to the receiver.")
                ),
                (
                    "receiver",
                    s("The peer name to send to (from buddy_peer_list).")
                ),
                (
                    "pruneKeep",
                    n("How many chains to keep on the receiver (0 = no prune).")
                ),
                ("raw", b("Send a raw stream (for ZFS-to-ZFS restores).")),
                ("force", b("Resend even when the peer seems ahead.")),
            ],
        )
    ),
    tool!(
        "buddy_peer_add",
        "Enroll a backup peer (its public key, allowed sources, quota).",
        Write,
        POST,
        "/api/buddy/peers",
        || schema(
            &["name", "publicKey"],
            &[
                (
                    "name",
                    s("The peer name (how the instance identifies itself when enrolling).")
                ),
                ("publicKey", s("The peer's Ed25519 public key (base64).")),
                (
                    "allowedSources",
                    s("Optional: restrict which source names the peer may present.")
                ),
                (
                    "quotaBytes",
                    n("Storage quota for this peer (0 = unlimited).")
                ),
                ("enabled", b("Whether the peer may connect (default true).")),
            ],
        )
    ),
    tool!(
        "buddy_schedule_create",
        "Create a recurring backup schedule for one dataset.",
        Write,
        POST,
        "/api/buddy/schedules",
        || schema(
            &["id", "dataset", "source", "receivers"],
            &[
                ("id", s("A unique schedule id.")),
                ("dataset", s("The dataset to back up.")),
                ("source", s("The source name presented to receivers.")),
                (
                    "receivers",
                    s("One receiver name or a comma-separated list.")
                ),
                (
                    "cadence",
                    s_enum(
                        "How often to run.",
                        &["hourly", "daily", "weekly", "monthly"]
                    )
                ),
                (
                    "runAt",
                    s("The run time (HH:MM for daily, day name or date for others).")
                ),
                ("pruneKeep", n("How many chains to keep on the receiver.")),
            ],
        )
    ),
    tool!(
        "buddy_restore",
        "Restore a dataset from a peer's backup. Overwrites the local dataset.",
        Full,
        POST,
        "/api/buddy/restore",
        confirm = Confirm::Arg("dataset"),
        || schema(
            &["source", "receiver", "dataset"],
            &[
                ("source", s("The source name the backup was sent under.")),
                ("receiver", s("The peer holding the backup.")),
                (
                    "dataset",
                    s("The local dataset (pool/name) to restore into.")
                ),
                (
                    "chain",
                    s("Optional specific chain id; default the newest.")
                ),
                ("force", b("Overwrite even if the local dataset exists.")),
                ("verify", b("Verify chunk digests during the restore.")),
                confirm_field(),
            ],
        )
    ),
    tool!(
        "buddy_peer_delete",
        "Remove an enrolled peer and its stored backups.",
        Full,
        DELETE,
        "/api/buddy/peers",
        query = ["name"],
        confirm = Confirm::Arg("name"),
        || schema(
            &["name"],
            &[("name", s("The peer to delete.")), confirm_field()]
        )
    ),
    tool!(
        "buddy_schedule_delete",
        "Delete a backup schedule.",
        Full,
        DELETE,
        "/api/buddy/schedules",
        query = ["id"],
        confirm = Confirm::Arg("id"),
        || schema(
            &["id"],
            &[("id", s("The schedule id to delete.")), confirm_field()]
        )
    ),
    tool!(
        "buddy_job_cancel",
        "Cancel a running backup job.",
        Full,
        DELETE,
        "/api/buddy/jobs/{id}",
        confirm = Confirm::Arg("id"),
        || schema(
            &["id"],
            &[("id", s("The job id to cancel.")), confirm_field()]
        )
    ),
    tool!(
        "buddy_identity_create",
        "Create (or replace) this instance's backup identity key.",
        Full,
        POST,
        "/api/buddy/identity",
        confirm = Confirm::Arg("name"),
        || schema(
            &["name"],
            &[
                ("name", s("The identity name to create."),),
                (
                    "replace",
                    b("Replace an existing identity (old receivers must re-enroll).")
                ),
                confirm_field(),
            ],
        )
    ),
];
