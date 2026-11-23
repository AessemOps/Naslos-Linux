//! App catalog, installed apps and chart-source tools (FR-MCP). Installing
//! and uninstalling apps are long-running jobs: the response carries the job
//! id, polled with `app_job_get`.

use super::{
    arr, b, confirm_field, free_map, n, no_args, s, s_enum, schema, Confirm, Method, Scope,
    Special, ToolDef,
};

pub const TOOLS: &[ToolDef] = &[
    tool!(
        "catalog_list",
        "The whole installable app catalog (every source and channel).",
        Read,
        GET,
        "/api/catalog",
        no_args
    ),
    tool!(
        "catalog_get",
        "One catalog app: versions, form schema, services and exposure settings.",
        Read,
        GET,
        "/api/catalog/{name}",
        || schema(&["name"], &[("name", s("The catalog app name."))])
    ),
    tool!(
        "app_list",
        "The installed apps with status, version and route.",
        Read,
        GET,
        "/api/apps",
        no_args
    ),
    tool!(
        "app_get",
        "One installed app: values, exposure, datasets, pods summary.",
        Read,
        GET,
        "/api/apps/{name}",
        || schema(&["name"], &[("name", s("The installed app name."))])
    ),
    tool!(
        "app_job_list",
        "The recent install/upgrade/uninstall jobs and their states.",
        Read,
        GET,
        "/api/apps/jobs",
        no_args
    ),
    tool!(
        "app_job_get",
        "One app job's progress and log tail.",
        Read,
        GET,
        "/api/apps/jobs/{id}",
        || schema(&["id"], &[("id", s("The job id from app_install/app_update/app_uninstall."))])
    ),
    tool!(
        "app_updates",
        "Which installed apps have newer chart versions available.",
        Read,
        GET,
        "/api/apps/updates",
        no_args
    ),
    tool!(
        "app_usage",
        "Per-app storage usage of their datasets.",
        Read,
        GET,
        "/api/apps/usage",
        no_args
    ),
    tool!(
        "app_services",
        "One installed app's services and ports.",
        Read,
        GET,
        "/api/apps/{name}/services",
        || schema(&["name"], &[("name", s("The installed app name."))])
    ),
    tool!(
        "app_pods",
        "One installed app's pods and containers (for pod_logs).",
        Read,
        GET,
        "/api/apps/{name}/pods",
        || schema(&["name"], &[("name", s("The installed app name."))])
    ),
    tool!(
        "app_quota",
        "One app's dataset quota settings.",
        Read,
        GET,
        "/api/apps/{name}/quota",
        || schema(&["name"], &[("name", s("The installed app name."))])
    ),
    tool!(
        "app_exposure_get",
        "One app's exposure settings (domains, ingress, TLS).",
        Read,
        GET,
        "/api/apps/{name}/exposure",
        || schema(&["name"], &[("name", s("The installed app name."))])
    ),
    tool!(
        "app_namespaces_get",
        "The per-app namespace pool settings.",
        Read,
        GET,
        "/api/apps/namespaces",
        no_args
    ),
    tool!(
        "source_list",
        "The configured chart sources (catalogs).",
        Read,
        GET,
        "/api/sources",
        no_args
    ),
    tool!(
        "source_get",
        "One chart source's URL, channels and sync state.",
        Read,
        GET,
        "/api/sources/{name}",
        || schema(&["name"], &[("name", s("The source name."))])
    ),
    tool!(
        "app_install",
        "Install a catalog app. Long-running: the response returns a job id to poll with app_job_get.",
        Write,
        POST,
        "/api/apps",
        || schema(
            &["name"],
            &[
                ("name", s("The catalog app name.")),
                ("values", free_map("Chart values for the install (see catalog_get's form schema).")),
                ("exposure", free_map("Optional exposure patch: enabled, domain, tls.")),
                ("baseDomain", s("Optional base domain for the app's hostnames.")),
                ("confirmed", b("Accept a privileged app's isolation trade-off when asked.")),
                ("privilegedConfirmed", b("Required for privileged apps: acknowledge the review.")),
                ("datasets", arr("Registered dataset names to mount.", s("A registered dataset name."))),
                ("pool", s("Optional storage pool for the app's dataset.")),
                ("env", arr("Extra environment variables.", free_map("One env entry: name/value."))),
            ],
        )
    ),
    tool!(
        "app_update",
        "Upgrade an installed app (values patch and/or chart version). Long-running: poll app_job_get.",
        Write,
        PUT,
        "/api/apps/{name}",
        || schema(
            &["name"],
            &[
                ("name", s("The installed app to upgrade.")),
                ("values", free_map("Chart values to change.")),
                ("confirmed", b("Accept a privileged move when asked.")),
            ],
        )
    ),
    tool!(
        "app_exposure_set",
        "Change an app's exposure (domain, enabled, TLS).",
        Write,
        PUT,
        "/api/apps/{name}/exposure",
        || schema(
            &["name"],
            &[
                ("name", s("The installed app name.")),
                ("exposure", free_map("The exposure patch: enabled, domain, tls.")),
                ("baseDomain", s("The base domain for the app's hostnames.")),
            ],
        )
    ),
    tool!(
        "app_dns_reconcile",
        "Re-push the app's DNS records and Traefik route (repairs drift).",
        Write,
        POST,
        "/api/apps/{name}/dns/reconcile",
        || schema(&["name"], &[("name", s("The installed app name."))])
    ),
    tool!(
        "app_namespaces_set",
        "Set the per-app namespace pool limit.",
        Write,
        PUT,
        "/api/apps/namespaces",
        || schema(&["limit"], &[("limit", n("The maximum isolated namespaces (pool size)."))])
    ),
    tool!(
        "source_add",
        "Add a chart source (a git repository with channels).",
        Full,
        POST,
        "/api/sources",
        || schema(
            &["name", "url"],
            &[
                ("name", s("The source name (unique).")),
                ("displayName", s("Optional display name.")),
                ("url", s("The git repository URL.")),
                ("auth", s_enum("How to authenticate.", &["public", "token", "ssh"])),
                ("credentialsSecret", s("The Kubernetes Secret holding credentials (token/ssh auth).")),
                ("channels", free_map("Channel map, e.g. {\"Prod\": \"main\"}.")),
            ],
        )
    ),
    tool!(
        "source_refresh",
        "Refresh every chart source now and rebuild the catalog.",
        Full,
        POST,
        "/api/sources/refresh",
        no_args
    ),
    tool!(
        "app_uninstall",
        "Uninstall an app and delete its datasets (destructive).",
        Full,
        DELETE,
        "/api/apps/{name}",
        confirm = Confirm::Arg("name"),
        || schema(&["name"], &[("name", s("The app to uninstall.")), confirm_field()])
    ),
    tool!(
        "source_delete",
        "Delete a chart source and its cached catalog entries.",
        Full,
        DELETE,
        "/api/sources/{name}",
        confirm = Confirm::Arg("name"),
        || schema(&["name"], &[("name", s("The source to delete.")), confirm_field()])
    ),
];
