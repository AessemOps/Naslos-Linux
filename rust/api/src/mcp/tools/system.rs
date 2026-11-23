//! System/dashboard tools (FR-MCP). The upgrade and reboot tools are the
//! destructive pair; `confirm` echoes the target version / the literal
//! `reboot`.

use super::{
    confirm_field, confirm_literal, no_args, s, s_enum, schema, Confirm, Method, Scope, Special,
    ToolDef,
};

pub const TOOLS: &[ToolDef] = &[
    tool!(
        "dashboard_get",
        "The dashboard summary: capacity, pools, services, alerts.",
        Read,
        GET,
        "/api/dashboard",
        no_args
    ),
    tool!(
        "metrics_get",
        "The current host metrics snapshot (CPU, memory, disks, ZFS pools).",
        Read,
        GET,
        "/api/metrics",
        no_args
    ),
    tool!(
        "system_version_get",
        "The running Naslos chart version, Talos and Kubernetes versions.",
        Read,
        GET,
        "/api/system/version",
        no_args
    ),
    tool!(
        "system_updates_get",
        "The last release-update check: the available candidate and blockers.",
        Read,
        GET,
        "/api/system/updates",
        no_args
    ),
    tool!(
        "system_operation_get",
        "The current system operation (upgrade) status, if one is in flight.",
        Read,
        GET,
        "/api/system/operations/current",
        no_args
    ),
    tool!(
        "system_updates_check",
        "Re-check for a newer Naslos release now (refreshes system_updates_get).",
        Full,
        POST,
        "/api/system/updates/check",
        no_args
    ),
    tool!(
        "system_upgrade_start",
        "Start a system upgrade to a released Naslos version. Long-running: poll system_operation_get.",
        Full,
        POST,
        "/api/system/operations",
        confirm = Confirm::Arg("version"),
        || schema(
            &["op", "version"],
            &[
                ("op", s_enum("The operation to start.", &["naslos", "talos", "kubernetes"])),
                ("version", s("The target release version, e.g. \"0.4.4\".")),
                confirm_field(),
            ],
        )
    ),
    tool!(
        "system_reboot",
        "Reboot the host node now. Every workload restarts.",
        Full,
        POST,
        "/api/system/reboot",
        confirm = Confirm::Literal("reboot"),
        || schema(&["confirm"], &[confirm_literal("reboot")])
    ),
];
