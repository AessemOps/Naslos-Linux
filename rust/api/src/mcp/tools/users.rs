//! User and group tools (FR-MCP). `user_enable`/`user_disable` are separate
//! tools over the `{action}` route so the registry stays a pure table.

use super::{arr, confirm_field, no_args, s, schema, Confirm, Method, Scope, Special, ToolDef};

pub const TOOLS: &[ToolDef] = &[
    tool!(
        "user_list",
        "Every LDAP user with display name, email and groups.",
        Read,
        GET,
        "/api/users",
        no_args
    ),
    tool!(
        "user_get",
        "One user's LDAP record.",
        Read,
        GET,
        "/api/users/{uid}",
        || schema(&["uid"], &[("uid", s("The user's uid/username."))])
    ),
    tool!(
        "group_list",
        "Every LDAP group with its members.",
        Read,
        GET,
        "/api/groups",
        no_args
    ),
    tool!(
        "group_get",
        "One group's record and members.",
        Read,
        GET,
        "/api/groups/{cn}",
        || schema(&["cn"], &[("cn", s("The group name."))])
    ),
    tool!(
        "user_create",
        "Create a user (LDAP + the Samba account mirror).",
        Write,
        POST,
        "/api/users",
        || schema(
            &["uid", "password"],
            &[
                ("uid", s("The username (lowercase, unique).")),
                ("displayName", s("The full display name.")),
                ("email", s("The user's email address.")),
                ("firstName", s("First name.")),
                ("lastName", s("Last name.")),
                (
                    "password",
                    s("The initial password (never returned by any read tool).")
                ),
                (
                    "groups",
                    arr("Groups the user belongs to.", s("A group name."))
                ),
            ],
        )
    ),
    tool!(
        "user_update",
        "Change one user's profile and group membership.",
        Write,
        PUT,
        "/api/users/{uid}",
        || schema(
            &["uid"],
            &[
                ("uid", s("The user to update.")),
                ("displayName", s("The full display name.")),
                ("email", s("The user's email address.")),
                ("firstName", s("First name.")),
                ("lastName", s("Last name.")),
                (
                    "groups",
                    arr(
                        "The complete group list (replaces the current one).",
                        s("A group name.")
                    )
                ),
            ],
        )
    ),
    tool!(
        "user_set_password",
        "Set a user's password (SMB mirror included).",
        Write,
        POST,
        "/api/users/{uid}/password",
        || schema(
            &["uid", "password"],
            &[
                ("uid", s("The user whose password changes.")),
                (
                    "password",
                    s("The new password (never returned by any read tool).")
                ),
            ],
        )
    ),
    tool!(
        "user_enable",
        "Enable a user's LDAP and Samba account.",
        Write,
        POST,
        "/api/users/{uid}/enable",
        || schema(&["uid"], &[("uid", s("The user to enable."))])
    ),
    tool!(
        "user_disable",
        "Disable a user's LDAP and Samba account (keeps the records).",
        Write,
        POST,
        "/api/users/{uid}/disable",
        || schema(&["uid"], &[("uid", s("The user to disable."))])
    ),
    tool!(
        "group_create",
        "Create an LDAP group.",
        Write,
        POST,
        "/api/groups",
        || schema(
            &["cn"],
            &[
                ("cn", s("The group name.")),
                ("description", s("Optional human description.")),
            ],
        )
    ),
    tool!(
        "group_update",
        "Set a group's member list (replaces the current membership).",
        Write,
        PUT,
        "/api/groups/{cn}",
        || schema(
            &["cn"],
            &[
                ("cn", s("The group to update.")),
                (
                    "members",
                    arr("The complete member list (uid values).", s("A user uid."))
                ),
            ],
        )
    ),
    tool!(
        "user_delete",
        "Delete a user's LDAP and Samba records permanently.",
        Full,
        DELETE,
        "/api/users/{uid}",
        confirm = Confirm::Arg("uid"),
        || schema(
            &["uid"],
            &[("uid", s("The user to delete.")), confirm_field()]
        )
    ),
    tool!(
        "group_delete",
        "Delete an LDAP group permanently.",
        Full,
        DELETE,
        "/api/groups/{cn}",
        confirm = Confirm::Arg("cn"),
        || schema(
            &["cn"],
            &[("cn", s("The group to delete.")), confirm_field()]
        )
    ),
];
