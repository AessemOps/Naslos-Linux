//! Share tools (FR-MCP). Folder tools operate on the shareable dataset
//! mountpoints; the config getters return the rendered Samba/Ganesha state.

use super::{
    arr, b, confirm_field, no_args, s, s_enum, schema, Confirm, Method, Scope, Special, ToolDef,
};

pub const TOOLS: &[ToolDef] = &[
    tool!(
        "share_list",
        "Every share definition (SMB/NFS) with its settings.",
        Read,
        GET,
        "/api/shares",
        no_args
    ),
    tool!(
        "share_get",
        "One share definition.",
        Read,
        GET,
        "/api/shares/{name}",
        || schema(&["name"], &[("name", s("The share name."))])
    ),
    tool!(
        "share_status",
        "Share service status: the rendered revision, share count and agent state.",
        Read,
        GET,
        "/api/shares/status",
        no_args
    ),
    tool!(
        "share_paths",
        "The dataset mountpoints shares may point at.",
        Read,
        GET,
        "/api/shares/paths",
        no_args
    ),
    tool!(
        "share_folder_list",
        "The folders that exist under one shareable dataset path.",
        Read,
        GET,
        "/api/shares/folders",
        query = ["path"],
        || schema(&["path"], &[("path", s("The dataset mountpoint to list."))])
    ),
    tool!(
        "share_config_get",
        "The rendered Samba or NFS server configuration.",
        Read,
        GET,
        "/api/shares/config/{service}",
        || schema(
            &["service"],
            &[(
                "service",
                s_enum("Which config to render.", &["samba", "nfs"])
            )]
        )
    ),
    tool!(
        "share_create",
        "Create a share over a dataset path (SMB or NFS).",
        Write,
        POST,
        "/api/shares",
        || schema(
            &["name", "path", "protocol"],
            &[
                ("name", s("The share name (SMB share name / export alias).")),
                (
                    "path",
                    s("The dataset mountpoint being shared (must exist).")
                ),
                (
                    "protocol",
                    s_enum("The share protocol.", &["smb", "nfs", "afp"])
                ),
                ("description", s("Optional human description.")),
                ("readOnly", b("Export read-only.")),
                ("browseable", b("SMB: visible in the network browser.")),
                (
                    "allowedHosts",
                    arr("NFS: hosts that may mount.", s("A host or CIDR."))
                ),
                (
                    "validUsers",
                    arr("SMB: users that may connect.", s("A user name."))
                ),
                (
                    "validGroups",
                    arr("SMB: groups that may connect.", s("A group name."))
                ),
                (
                    "timeMachine",
                    b("Advertise as a macOS Time Machine target.")
                ),
                ("noRootSquash", b("NFS: root keeps its privileges.")),
                (
                    "shadowCopies",
                    b("SMB: expose ZFS snapshots as Previous Versions.")
                ),
                (
                    "audit",
                    b("SMB: write share operations to the smbd audit log.")
                ),
            ],
        )
    ),
    tool!(
        "share_update",
        "Change one share's settings (any subset of the share_create fields).",
        Write,
        PUT,
        "/api/shares/{name}",
        || schema(
            &["name"],
            &[
                ("name", s("The share to update.")),
                ("description", s("Optional human description.")),
                ("readOnly", b("Export read-only.")),
                ("browseable", b("SMB: visible in the network browser.")),
                (
                    "allowedHosts",
                    arr("NFS: hosts that may mount.", s("A host or CIDR."))
                ),
                (
                    "validUsers",
                    arr("SMB: users that may connect.", s("A user name."))
                ),
                (
                    "validGroups",
                    arr("SMB: groups that may connect.", s("A group name."))
                ),
                (
                    "timeMachine",
                    b("Advertise as a macOS Time Machine target.")
                ),
                ("noRootSquash", b("NFS: root keeps its privileges.")),
                (
                    "shadowCopies",
                    b("SMB: expose ZFS snapshots as Previous Versions.")
                ),
                (
                    "audit",
                    b("SMB: write share operations to the smbd audit log.")
                ),
            ],
        )
    ),
    tool!(
        "share_apply",
        "Push the current share definitions to the running Samba/NFS services.",
        Write,
        POST,
        "/api/shares/apply",
        no_args
    ),
    tool!(
        "share_folder_create",
        "Create a folder inside a shareable dataset path.",
        Write,
        POST,
        "/api/shares/folders",
        || schema(
            &["path", "name"],
            &[
                ("path", s("The parent dataset mountpoint."),),
                ("name", s("The folder name to create.")),
            ],
        )
    ),
    tool!(
        "share_delete",
        "Delete a share definition (the dataset and its files are untouched).",
        Full,
        DELETE,
        "/api/shares/{name}",
        confirm = Confirm::Arg("name"),
        || schema(
            &["name"],
            &[("name", s("The share to delete.")), confirm_field()]
        )
    ),
    tool!(
        "share_folder_delete",
        "Delete a folder and everything inside it under a dataset path.",
        Full,
        DELETE,
        "/api/shares/folders",
        query = ["path"],
        confirm = Confirm::Arg("path"),
        || schema(
            &["path"],
            &[("path", s("The folder's dataset path.")), confirm_field()]
        )
    ),
];
