//! Host-network tools (FR-MCP). Applying a network change reconfigures the
//! node's interface and can sever the UI connection; reverting is the escape
//! hatch. Both are `full`: applying confirms the link **and** the whole
//! payload hash, reverting confirms the change id.

use super::{
    arr, b, confirm_field, confirm_payload, free_map, n, no_args, s, s_enum, schema, Confirm,
    Method, Scope, Special, ToolDef,
};

pub const TOOLS: &[ToolDef] = &[
    tool!(
        "network_get",
        "The current host network configuration (links, addressing, DNS).",
        Read,
        GET,
        "/api/network",
        no_args
    ),
    tool!(
        "network_change_get",
        "One pending/applied network change record by id.",
        Read,
        GET,
        "/api/network/changes/{id}",
        || schema(&["id"], &[("id", s("The change id."))])
    ),
    tool!(
        "network_change_apply",
        "Start a host network change (long-running; poll network_change_get). Can disconnect the node if the values are wrong. Confirm the link and the whole payload.",
        Full,
        POST,
        "/api/network/changes",
        confirm = Confirm::Arg("link"),
        || schema(
            &["link"],
            &[
                ("link", s("The network link the change applies to (from network_get).")),
                ("addressing", s_enum("The addressing mode.", &["dhcp", "static"])),
                ("addresses", arr("Static addresses with prefix, e.g. \"192.168.1.10/24\".", s("A CIDR address."))),
                ("gateway", s("The default gateway address.")),
                ("mtu", n("The link MTU (needs confirmLowMtu below 1500).")),
                ("confirmLowMtu", b("Accept an MTU below 1500.")),
                ("nameservers", free_map("DNS nameservers: {\"mode\": \"static\"|\"dhcp\", \"servers\": [...]}."),
                ),
                ("searchDomains", free_map("DNS search domains: {\"mode\": …, \"domains\": [...]}."),
                ),
                confirm_payload("link"),
            ],
        )
    ),
    tool!(
        "network_change_revert",
        "Revert an applied network change back to the previous configuration.",
        Full,
        POST,
        "/api/network/changes/{id}/revert",
        confirm = Confirm::Arg("id"),
        || schema(&["id"], &[("id", s("The applied change to revert.")), confirm_field()])
    ),
];
