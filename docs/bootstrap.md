# Bootstrap & the Talos Image

Naslos does **not** fork Talos. A single Image Factory schematic extends stock
Talos with the official ZFS extension; everything else is Kubernetes.

## The schematic

`bootstrap/schematic/naslos.yaml`:

```yaml
customization:
  systemExtensions:
    officialExtensions:
      - siderolabs/zfs
```

The `siderolabs/zfs` extension provides:

- a signed ZFS kernel module for the Talos kernel, and
- the `zfs-service` systemd unit, which runs `zpool import -fal` at boot and
  `zfs unmount -au` + `zpool export -a` at shutdown.

## Building a Naslos image

```bash
# Render the schematic (prints the generated machine image URL)
talosctl image factory schematic render \
  --schematic bootstrap/schematic/naslos.yaml

# Or bundle an installer tarball
make bootstrap        # = talosctl image factory schematic bundle …
```

`make dev-cluster` builds a local cluster from the rendered image:

```bash
make dev-cluster
# talosctl cluster create --name naslos-dev \
#   --image factory.talos.dev/<rendered digest> --workers 0
```

### The pinned ISO (installer)

The validated schematic is
`4dd8e3a8b6203d3c14f049da8db4d3bb0d6d3e70c5e89dfcc1e709e81914f63c`, with
`siderolabs/zfs` at Talos **v1.14.1** (amd64 only). The boot ISO the desktop
installer points at is

```
https://factory.talos.dev/image/4dd8e3a8b6203d3c14f049da8db4d3bb0d6d3e70c5e89dfcc1e709e81914f63c/v1.14.1/metal-amd64.iso
```

(likewise the installer image
`factory.talos.dev/installer/<schematic>:v1.14.1`). Both are in the install
pack's `metadata.json`; see [installer-contract.md](installer-contract.md).

## First boot sequence

1. Stock Talos boots with the ZFS extension.
2. `zfs-service` imports any pools found on disks (`zpool import -fal`).
3. `naslos-agent` (DaemonSet) verifies ZFS availability and listens on `:9090`.
4. `naslos-api` connects to Talos (machine API), Kubernetes, and LDAP.
5. The OpenLDAP bootstrap Job sets the service password and verifies groups.
6. The admin creates a first user through the API (the flow and payload are in
   [identity-sso.md](identity-sso.md#deployment--first-admin)).

Because no Talos base is modified, images stay reproducible and concise; the
schematic is the only thing that changes the OS image.

## Upgrades

- **Talos**: `talosctl upgrade` upgrades the OS. The extension is re-imported
  from the Image Factory; pools are re-imported by `zfs-service` on the new boot.
- **Naslos**: Helm chart upgrades (`helm upgrade … charts/naslos`) redeploy the
  API/UI and agent workloads; machine-config patches are applied out-of-band with
  `talosctl apply-config` / `scripts/deploy-vm.sh` (the API does not reapply
  them).

## Where it lives

| Path | Role |
| --- | --- |
| `bootstrap/schematic/naslos.yaml` | The Image Factory schematic |
| `bootstrap/vm/naslos-vm.yaml` | Machine-config patch for the hand-managed VM |
| `bootstrap/installer/naslos-installer.yaml.tmpl` | Parameterised patch shipped in the install pack (FR-INSTALL) |
| `bootstrap/cilium/cilium.yaml` | Cilium manifest inlined into both patches |
| `Makefile` targets `bootstrap`, `dev-cluster`, `install-pack` | Build, local dev cluster, install pack |
| `charts/naslos/` | Umbrella Helm chart (installs on top of the image) |