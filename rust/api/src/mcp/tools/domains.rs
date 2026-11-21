//! Domain, certificate and dynamic-DNS tools (FR-MCP).

use super::{
    b, confirm_field, n, no_args, s, s_enum, schema, str_map, Confirm, Method, Scope, Special,
    ToolDef,
};

pub const TOOLS: &[ToolDef] = &[
    tool!(
        "domain_list",
        "The configured domains (primary and SSO) with certificate status.",
        Read,
        GET,
        "/api/domains",
        no_args
    ),
    tool!(
        "domain_get",
        "One domain's settings: DNS provider, certificate state, SSO flag.",
        Read,
        GET,
        "/api/domains/{name}",
        || schema(&["name"], &[("name", s("The domain name."))])
    ),
    tool!(
        "domain_certificate_get",
        "One domain's certificate details (issuer, validity, san entries).",
        Read,
        GET,
        "/api/domains/{name}/certificate",
        || schema(&["name"], &[("name", s("The domain name."))])
    ),
    tool!(
        "provider_list",
        "The available DNS providers and their required credential fields.",
        Read,
        GET,
        "/api/providers",
        no_args
    ),
    tool!(
        "ddns_list",
        "The configured dynamic-DNS entries and their last update result.",
        Read,
        GET,
        "/api/ddns",
        no_args
    ),
    tool!(
        "ddns_get",
        "One dynamic-DNS entry.",
        Read,
        GET,
        "/api/ddns/{id}",
        || schema(&["id"], &[("id", s("The DDNS entry id."))])
    ),
    tool!(
        "domain_create",
        "Add a domain (with its DNS provider credentials for certificates).",
        Write,
        POST,
        "/api/domains",
        || schema(
            &["baseDomain"],
            &[
                ("baseDomain", s("The domain name, e.g. \"example.com\".")),
                (
                    "dnsProvider",
                    s_enum(
                        "Who resolves this domain's DNS records.",
                        &[
                            "ovh",
                            "cloudflare",
                            "digitalocean",
                            "godaddy",
                            "porkbun",
                            "http"
                        ],
                    )
                ),
                (
                    "credentialsSecret",
                    s("The Kubernetes Secret with the provider credentials.")
                ),
                (
                    "providerConfig",
                    str_map("Extra provider settings (zone id, …).")
                ),
            ],
        )
    ),
    tool!(
        "domain_update",
        "Change a domain's DNS provider or credentials.",
        Write,
        PUT,
        "/api/domains/{name}",
        || schema(
            &["name"],
            &[
                ("name", s("The domain to update.")),
                (
                    "dnsProvider",
                    s_enum(
                        "Who resolves this domain's DNS records.",
                        &[
                            "ovh",
                            "cloudflare",
                            "digitalocean",
                            "godaddy",
                            "porkbun",
                            "http"
                        ],
                    )
                ),
                (
                    "credentialsSecret",
                    s("The Kubernetes Secret with the provider credentials.")
                ),
                (
                    "providerConfig",
                    str_map("Extra provider settings (zone id, …).")
                ),
            ],
        )
    ),
    tool!(
        "domain_sso_set",
        "Make a domain an Authelia SSO domain (or stop using it as one).",
        Write,
        POST,
        "/api/domains/{name}/sso",
        || schema(
            &["name", "enabled"],
            &[
                ("name", s("The domain (not the primary).")),
                ("enabled", b("Whether apps may use this domain for SSO.")),
            ],
        )
    ),
    tool!(
        "ddns_create",
        "Create a dynamic-DNS record that follows the public IP.",
        Write,
        POST,
        "/api/ddns",
        || schema(
            &["provider", "zone", "record"],
            &[
                ("provider", s("The DNS provider (see provider_list).")),
                ("zone", s("The DNS zone the record lives in.")),
                (
                    "record",
                    s("The record name, e.g. \"naslos\" (becomes naslos.zone).")
                ),
                ("recordType", s_enum("The record type.", &["A", "AAAA"])),
                ("ttl", n("Record TTL in seconds.")),
                ("enabled", b("Whether updates run (default true).")),
                (
                    "fields",
                    str_map("Provider credential fields (see provider_list).")
                ),
            ],
        )
    ),
    tool!(
        "ddns_update",
        "Change a dynamic-DNS entry (any subset of ddns_create fields).",
        Write,
        PUT,
        "/api/ddns/{id}",
        || schema(
            &["id"],
            &[
                ("id", s("The DDNS entry to update.")),
                ("provider", s("The DNS provider (see provider_list).")),
                ("zone", s("The DNS zone the record lives in.")),
                ("record", s("The record name.")),
                ("recordType", s_enum("The record type.", &["A", "AAAA"])),
                ("ttl", n("Record TTL in seconds.")),
                ("enabled", b("Whether updates run.")),
                (
                    "fields",
                    str_map("Provider credential fields (see provider_list).")
                ),
            ],
        )
    ),
    tool!(
        "ddns_run",
        "Run one dynamic-DNS update now (don't wait for the interval).",
        Write,
        POST,
        "/api/ddns/{id}/run",
        || schema(&["id"], &[("id", s("The DDNS entry to run."))])
    ),
    tool!(
        "domain_delete",
        "Delete a domain, its certificate and its DNS reconciliation.",
        Full,
        DELETE,
        "/api/domains/{name}",
        confirm = Confirm::Arg("name"),
        || schema(
            &["name"],
            &[("name", s("The domain to delete.")), confirm_field()]
        )
    ),
    tool!(
        "ddns_delete",
        "Delete a dynamic-DNS entry.",
        Full,
        DELETE,
        "/api/ddns/{id}",
        confirm = Confirm::Arg("id"),
        || schema(
            &["id"],
            &[("id", s("The DDNS entry to delete.")), confirm_field()]
        )
    ),
];
