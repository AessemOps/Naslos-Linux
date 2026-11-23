//! VM tools (FR-MCP). Image upload is deliberately NOT a tool (binary
//! streams); importing by URL is supported. Destructive VM tools confirm on
//! the VM name; snapshot restore confirms the VM **and** the snapshot.

use super::{
    b, confirm_field, confirm_tuple, free_map, n, no_args, s, s_enum, schema, Confirm, Method,
    Scope, Special, ToolDef,
};

pub const TOOLS: &[ToolDef] = &[
    tool!(
        "vm_capabilities",
        "What the QEMU supervisor supports (firmware, image formats, limits).",
        Read,
        GET,
        "/api/virt/capabilities",
        no_args
    ),
    tool!(
        "vm_list",
        "The defined VMs with their desired state.",
        Read,
        GET,
        "/api/virt/vms",
        no_args
    ),
    tool!(
        "vm_get",
        "One VM's definition (cpu, memory, disk, network, snapshots).",
        Read,
        GET,
        "/api/virt/vms/{name}",
        || schema(&["name"], &[("name", s("The VM name."))])
    ),
    tool!(
        "vm_snapshot_list",
        "One VM's ZFS snapshots.",
        Read,
        GET,
        "/api/virt/vms/{name}/snapshots",
        || schema(&["name"], &[("name", s("The VM name."))])
    ),
    tool!(
        "vm_image_list",
        "The image library (imported ISOs/disk images) and in-flight imports.",
        Read,
        GET,
        "/api/virt/images",
        no_args
    ),
    tool!(
        "vm_create",
        "Create a VM (CPU, memory, disk, network, optional cloud-init/ISO).",
        Write,
        POST,
        "/api/virt/vms",
        || schema(
            &["name", "pool"],
            &[
                ("name", s("The VM name (a dataset component).")),
                ("cpu", n("vCPUs (default 1).")),
                ("memMiB", n("Memory in MiB (default 2048).")),
                ("autostart", b("Start with the host.")),
                ("firmware", s_enum("The boot firmware.", &["uefi", "bios"])),
                (
                    "secureBoot",
                    b("Boot with OVMF Secure Boot (requires UEFI firmware).")
                ),
                (
                    "desired",
                    s_enum("The desired power state.", &["running", "stopped"])
                ),
                (
                    "disk",
                    free_map("Disk settings: sizeGb, format (qcow2|raw).")
                ),
                (
                    "net",
                    free_map(
                        "Network settings: portForwards[] (NAT only; node port → guest port)."
                    )
                ),
                (
                    "cloudInit",
                    free_map("Optional cloud-init: user, sshAuthorizedKeys, ….")
                ),
                (
                    "iso",
                    s("Optional library file to attach as CD-ROM (isos/<file>).")
                ),
                ("pool", s("The ZFS pool the VM dataset lives in.")),
            ],
        )
    ),
    tool!(
        "vm_update",
        "Change one VM's definition (any subset of vm_create fields).",
        Write,
        PUT,
        "/api/virt/vms/{name}",
        || schema(
            &["name"],
            &[
                ("name", s("The VM to update.")),
                ("cpu", n("vCPUs.")),
                ("memMiB", n("Memory in MiB.")),
                ("autostart", b("Start with the host.")),
                ("firmware", s_enum("The boot firmware.", &["uefi", "bios"])),
                (
                    "secureBoot",
                    b("Boot with OVMF Secure Boot (requires UEFI firmware).")
                ),
                (
                    "desired",
                    s_enum("The desired power state.", &["running", "stopped"])
                ),
                (
                    "iso",
                    s("Optional library file to attach as CD-ROM (isos/<file>).")
                ),
            ],
        )
    ),
    tool!(
        "vm_start",
        "Start a stopped VM.",
        Write,
        POST,
        "/api/virt/vms/{name}/start",
        || schema(&["name"], &[("name", s("The VM name."))])
    ),
    tool!(
        "vm_stop",
        "Gracefully stop a running VM (ACPI).",
        Write,
        POST,
        "/api/virt/vms/{name}/stop",
        || schema(&["name"], &[("name", s("The VM name."))])
    ),
    tool!(
        "vm_restart",
        "Gracefully stop then start a VM.",
        Write,
        POST,
        "/api/virt/vms/{name}/restart",
        || schema(&["name"], &[("name", s("The VM name."))])
    ),
    tool!(
        "vm_snapshot_create",
        "Take a ZFS snapshot of a VM's dataset.",
        Write,
        POST,
        "/api/virt/vms/{name}/snapshots",
        || schema(
            &["name", "snapshot"],
            &[
                ("name", s("The VM name.")),
                (
                    "snapshot",
                    s("The snapshot name (its ZFS snapshot component).")
                ),
            ],
        )
    ),
    tool!(
        "vm_image_import_url",
        "Import a disk image/ISO into the library from a public http(s) URL.",
        Full,
        POST,
        "/api/virt/images/import",
        || schema(
            &["url", "name", "pool"],
            &[
                ("url", s("The public http(s) URL to download.")),
                (
                    "name",
                    s("The library file name (e.g. \"debian-13.iso\", stored under isos/).")
                ),
                (
                    "pool",
                    s("The pool whose dataset stores the image library.")
                ),
            ],
        )
    ),
    tool!(
        "vm_force_stop",
        "Kill a VM immediately (like pulling the power). Data loss possible.",
        Full,
        POST,
        "/api/virt/vms/{name}/force-stop",
        confirm = Confirm::Arg("name"),
        || schema(&["name"], &[("name", s("The VM name.")), confirm_field()])
    ),
    tool!(
        "vm_delete",
        "Delete a VM and its dataset (all disks and snapshots).",
        Full,
        DELETE,
        "/api/virt/vms/{name}",
        confirm = Confirm::Arg("name"),
        || schema(
            &["name"],
            &[("name", s("The VM to delete.")), confirm_field()]
        )
    ),
    tool!(
        "vm_snapshot_restore",
        "Roll a VM's dataset back to a snapshot (changes since the snapshot are lost). Confirm both the VM and the snapshot.",
        Full,
        POST,
        "/api/virt/vms/{name}/snapshots/{snap}/restore",
        confirm = Confirm::Arg("name"),
        || schema(
            &["name", "snap"],
            &[
                ("name", s("The VM name.")),
                (
                    "snap",
                    s("The snapshot to restore (from vm_snapshot_list).")
                ),
                confirm_tuple(&["name", "snap"]),
            ],
        )
    ),
    tool!(
        "vm_image_delete",
        "Delete an image library file.",
        Full,
        DELETE,
        "/api/virt/images/{name}",
        confirm = Confirm::Arg("name"),
        || schema(
            &["name"],
            &[("name", s("The library file to delete.")), confirm_field()]
        )
    ),
];
