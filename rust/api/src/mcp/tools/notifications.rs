//! Notification tools (FR-MCP). The settings write can change the ntfy auth
//! token, so the read tool never echoes it (the API returns hasAuthToken).

use super::{arr, b, no_args, s, s_enum, schema, Confirm, Method, Scope, Special, ToolDef};

pub const TOOLS: &[ToolDef] = &[
    tool!(
        "notifications_get",
        "The ntfy notification settings (the auth token is only reported as present).",
        Read,
        GET,
        "/api/notifications",
        no_args
    ),
    tool!(
        "notifications_set",
        "Update the notification settings.",
        Full,
        PUT,
        "/api/notifications",
        || schema(
            &[],
            &[
                ("enabled", b("Whether notifications are sent.")),
                ("serverUrl", s("The ntfy server URL.")),
                ("topic", s("The topic to publish to.")),
                (
                    "authToken",
                    s("The ntfy auth token (write-only, never returned).")
                ),
                ("email", s("Optional email forwarding via ntfy.")),
                (
                    "enabledEvents",
                    arr("Which events notify.", s("An event key, e.g. zfs_health."))
                ),
                (
                    "minSeverity",
                    s_enum(
                        "The minimum severity that notifies.",
                        &["info", "warning", "error", "critical"]
                    )
                ),
            ],
        )
    ),
    tool!(
        "notifications_test",
        "Send a test notification with the current settings.",
        Full,
        POST,
        "/api/notifications/test",
        no_args
    ),
];
