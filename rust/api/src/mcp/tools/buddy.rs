//! Buddy backup tools (FR-MCP). Sends are long-running jobs (poll with
//! buddy_job_get); restores overwrite local datasets and confirm on the
//! dataset name. `receiver` is always the receiver's base URL, never a peer
//! name (the peer label only exists on the receiver's own enrollment list).

use super::{
    arr, b, confirm_field, n, no_args, s, s_enum, schema, Confirm, Method, Scope, Special, ToolDef,
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
        "This instance's backup identity: public key, fingerprint and file path. It does not return receivers (use buddy_peer_list).",
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
        "Start a backup send of one dataset to a receiver URL. Arbitrary egress, so this is full scope. Long-running: poll buddy_job_get.",
        Full,
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
                    s("The receiver's base URL (http(s)://…), not a peer name.")
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
        Full,
        POST,
        "/api/buddy/peers",
        || schema(
            &["name", "publicKey"],
            &[
                (
                    "name",
                    s("The peer name (how the instance identifies itself when enrolling).")
                ),
                (
                    "publicKey",
                    s("The peer's OpenSSH public key line, e.g. \"ssh-ed25519 AAAA… comment\" (not a bare base64 blob).")
                ),
                (
                    "allowedSources",
                    arr(
                        "Optional source names the peer may present; an array of strings, not a JSON-encoded string.",
                        s("A source name, e.g. naslos-a/")
                    )
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
        "Create a recurring backup schedule for one dataset. Receiver URLs are arbitrary egress, so this is full scope.",
        Full,
        POST,
        "/api/buddy/schedules",
        || schema(
            &["dataset", "source"],
            &[
                ("id", s("Optional unique schedule id (generated when omitted).")),
                ("dataset", s("The dataset to back up.")),
                ("source", s("The source name presented to receivers.")),
                (
                    "receivers",
                    arr(
                        "One or more receiver base URLs (http(s)://…), as an array of strings.",
                        s("A receiver base URL.")
                    )
                ),
                (
                    "receiver",
                    s("Optional single receiver base URL; use receivers for several.")
                ),
                (
                    "cadence",
                    s_enum("How often to run.", &["hourly", "daily", "weekly"])
                ),
                (
                    "runAt",
                    s("The run time: HH:MM for daily, \"Day HH:MM\" (three-letter weekday) for weekly; ignored for hourly.")
                ),
                ("pruneKeep", n("How many chains to keep on the receiver.")),
                ("enabled", b("Whether the schedule runs (default true).")),
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
                (
                    "receiver",
                    s("The receiver's base URL (http(s)://…), not a peer name.")
                ),
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
        "Revoke an enrolled peer's key. Existing received backups are kept until they are pruned; this does not destroy them.",
        Full,
        DELETE,
        "/api/buddy/peers",
        query = ["name"],
        confirm = Confirm::Arg("name"),
        || schema(
            &["name"],
            &[("name", s("The peer to revoke.")), confirm_field()]
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
        confirm = Confirm::ArgOrLiteral {
            arg: "name",
            flag: "replace",
            literal: "replace",
        },
        || {
            schema(
            &["name"],
            &[
                ("name", s("The identity name to create."),),
                (
                    "replace",
                    b("Replace an existing identity (old receivers must re-enroll).")
                ),
                (
                    "confirm",
                    s("Safety check: repeat the identity name, or the literal \"replace\" when replace is true.")
                ),
            ],
        )
        }
    ),
];
