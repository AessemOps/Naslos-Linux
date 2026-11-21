//! ZFS dataset tools (FR-MCP): listing, creation, deletion and the
//! operator registry apps can be granted access to.

use super::{
    b, confirm_field, no_args, s, schema, str_map, Confirm, Method, Scope, Special, ToolDef,
};

pub const TOOLS: &[ToolDef] = &[
    tool!(
        "dataset_list",
        "Every ZFS dataset with usage, mountpoint and record size.",
        Read,
        GET,
        "/api/datasets",
        no_args
    ),
    tool!(
        "dataset_create",
        "Create a ZFS dataset (pool/name) with optional ZFS options.",
        Write,
        POST,
        "/api/datasets",
        || schema(
            &["pool", "name"],
            &[
                ("pool", s("The parent pool.")),
                (
                    "name",
                    s("The dataset name (its component under the pool).")
                ),
                (
                    "options",
                    str_map("Optional ZFS properties, e.g. {\"quota\": \"10G\"}.")
                ),
            ],
        )
    ),
    tool!(
        "dataset_delete",
        "Delete a ZFS dataset and everything stored under its mountpoint.",
        Full,
        DELETE,
        "/api/datasets",
        query = ["name", "recursive"],
        confirm = Confirm::Arg("name"),
        || schema(
            &["name"],
            &[
                ("name", s("The full dataset path (pool/name).")),
                (
                    "recursive",
                    b("Also delete child datasets (default false).")
                ),
                confirm_field(),
            ],
        )
    ),
    tool!(
        "dataset_registry_list",
        "The operator-registered datasets that apps can be granted access to.",
        Read,
        GET,
        "/api/datasets/registry",
        no_args
    ),
    tool!(
        "dataset_register",
        "Register an existing dataset mountpoint so apps can request it.",
        Write,
        POST,
        "/api/datasets/registry",
        || schema(
            &["name", "hostPath"],
            &[
                ("name", s("The registry entry name (DNS-like, unique).")),
                (
                    "hostPath",
                    s("The dataset's host mountpoint, e.g. /var/mnt/tank/media.")
                ),
                (
                    "shared",
                    b("Readable by every attached app (needs warningAcknowledged).")
                ),
                (
                    "warningAcknowledged",
                    b("Confirm you understand the shared-dataset exposure.")
                ),
            ],
        )
    ),
    tool!(
        "dataset_unregister",
        "Remove a dataset from the app-grantable registry (the dataset itself is untouched).",
        Full,
        DELETE,
        "/api/datasets/registry/{name}",
        confirm = Confirm::Arg("name"),
        || schema(
            &["name"],
            &[
                ("name", s("The registry entry to remove.")),
                confirm_field()
            ]
        )
    ),
];
