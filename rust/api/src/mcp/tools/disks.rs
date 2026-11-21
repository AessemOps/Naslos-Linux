//! Disk, pool and dataset tools (FR-MCP). Pool writes destroy data by
//! design; every destructive entry confirms on the pool/dataset name.

use super::{
    arr, b, confirm_field, no_args, s, s_enum, schema, str_map, Confirm, Method, Scope, Special,
    ToolDef,
};

pub const TOOLS: &[ToolDef] = &[
    tool!(
        "disk_list",
        "Every physical disk with its model, serial, size and current use.",
        Read,
        GET,
        "/api/disks",
        no_args
    ),
    tool!(
        "disk_health",
        "SMART health per disk (temperature, wear, errors).",
        Read,
        GET,
        "/api/disks/health",
        no_args
    ),
    tool!(
        "disk_recommend",
        "Get a topology recommendation for a set of (candidate) disks.",
        Read,
        POST,
        "/api/disks/recommend",
        || schema(
            &[],
            &[("disks", arr("Optional subset of disk devices to consider.", s("A disk device path.")))],
        )
    ),
    tool!(
        "pool_list",
        "The ZFS pools with health, size and allocation.",
        Read,
        GET,
        "/api/volumes/zfs",
        no_args
    ),
    tool!(
        "pool_get",
        "One pool's datasets, devices and statistics.",
        Read,
        GET,
        "/api/volumes/zfs/{pool}",
        || schema(&["pool"], &[("pool", s("The pool name."))])
    ),
    tool!(
        "pool_health",
        "One pool's health and status details.",
        Read,
        GET,
        "/api/volumes/zfs/{pool}/health",
        || schema(&["pool"], &[("pool", s("The pool name."))])
    ),
    tool!(
        "pool_importable_list",
        "Pools that exist on the disks but are not imported yet.",
        Read,
        GET,
        "/api/volumes/zfs/import",
        no_args
    ),
    tool!(
        "pool_import",
        "Import an existing (exported or foreign) ZFS pool by name.",
        Write,
        POST,
        "/api/volumes/zfs/import",
        || schema(&["name"], &[("name", s("The pool name to import."))])
    ),
    tool!(
        "pool_create",
        "Create a new ZFS pool on the given disks. This ERASES those disks unless they are listed in wipeConfirmed.",
        Full,
        POST,
        "/api/volumes/zfs",
        confirm = Confirm::Arg("name"),
        || schema(
            &["name", "topology", "disks"],
            &[
                ("name", s("The new pool name.")),
                ("topology", s_enum(
                    "The pool topology.",
                    &["stripe", "mirror", "raidz1", "raidz2", "raidz3"],
                )),
                ("disks", arr("The disk devices to use, e.g. \"/dev/sdb\".", s("A disk device path."))),
                ("cache", s("Optional cache device (e.g. a fast SSD).")),
                ("options", str_map("Optional extra ZFS properties for the pool.")),
                ("wipeConfirmed", arr("Disks whose data you accept to erase.", s("A disk device path."))),
                confirm_field(),
            ],
        )
    ),
    tool!(
        "pool_add_devices",
        "Add devices (mirror/raid group) to an existing pool. May erase newly added disks.",
        Full,
        POST,
        "/api/volumes/zfs/{pool}/devices",
        confirm = Confirm::Arg("pool"),
        || schema(
            &["pool", "disks", "topology"],
            &[
                ("pool", s("The pool to extend.")),
                ("disks", arr("The disk devices to add.", s("A disk device path."))),
                ("topology", s_enum("The layout of the added group.", &["stripe", "mirror", "raidz1", "raidz2", "raidz3"])),
                ("force", b("Force the attach even if in use.")),
                ("wipeConfirmed", arr("Disks whose data you accept to erase.", s("A disk device path."))),
                confirm_field(),
            ],
        )
    ),
    tool!(
        "pool_destroy",
        "Destroy a ZFS pool and LOSE its data permanently.",
        Full,
        DELETE,
        "/api/volumes/zfs/{pool}",
        confirm = Confirm::Arg("pool"),
        || schema(&["pool"], &[("pool", s("The pool to destroy.")), confirm_field()])
    ),
];
