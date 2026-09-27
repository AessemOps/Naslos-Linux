# Installer contract

Stable interfaces between **Naslos-Linux** and the desktop installer
(**Naslos-Installer**, a separate repository). Anything here is a contract: a
change to a name, path, payload, exec command or pack field must update this doc
**and** the installer in the same release (AGENTS "Docs rule"; spec
`FR-INSTALL`).

The installer does not import Naslos-Linux code. The only coupling is the
versioned **install pack** plus the interfaces below.

## 1. Install pack

Built by `make install-pack` (`scripts/build-install-pack.sh`), published by the
`install-pack` GitHub Actions workflow on a semantic **`vX.Y.Z`** tag, and
embedded by the installer: `scripts/fetch-install-pack.sh` resolves the **newest
`vX.Y.Z` tag** of this repo, downloads the tarball, verifies its sha256 (and the
loader then verifies every member checksum), and extracts it into a gitignored
`installpack/` that is `go:embed`-ed. The Makefile derives the installer's
`ExpectedTalosVersion`/`ExpectedSchematicID` gate from that pack's
`metadata.json`. Non-semver tags (e.g. a moving `latest`) are ignored.

After attaching the pack, the `install-pack` workflow sends a
`repository_dispatch` (`naslos-release`) to `AessemOps/Naslos-Installer`, so
publishing a pack rebuilds and republishes the installer against it (requires
the `INSTALLER_DISPATCH_TOKEN` secret in this repo).

`naslos-install-pack-<version>.tar.gz`:

```
naslos-install-pack-<version>/
  metadata.json
  charts/naslos/                       umbrella chart + charts/*.tgz subcharts
  charts/naslos/values-installer.yaml
  machine-config/naslos-installer.yaml.tmpl
  cilium/cilium.yaml
  manifests/local-path-v0.0.26.yaml
  schematic/naslos.yaml
```

`metadata.json`:

| Field | Meaning |
| --- | --- |
| `name` | `naslos-install-pack` |
| `formatVersion` | currently `1`; a change is a breaking pack change |
| `naslosVersion` | product/app version (`Chart.yaml` `appVersion`) |
| `chartVersion` | umbrella chart version |
| `chartAppVersion` | umbrella chart `appVersion` |
| `talosVersion` | Talos version the pack targets (e.g. `v1.14.1`) |
| `schematicId` | Image Factory schematic id for the ZFS installer/ISO |
| `isoUrls` | `{"metal-amd64": "https://factory.talos.dev/image/<schematicId>/<talosVersion>/metal-amd64.iso"}` |
| `generatedAt` | UTC build timestamp |
| `checksums` | map of every other member path → sha256 |

The installer MUST verify every `checksums` entry before use and MUST refuse a
pack whose `talosVersion`/`schematicId` differ from the values it was built to
expect. `naslosVersion` is recorded in the recovery README.

### 1.1 Machine-config template

`machine-config/naslos-installer.yaml.tmpl` is a Talos machine-config **patch**
(one or more YAML documents) applied on top of the config generated from
`pkg/machinery`. It contains exactly two engine placeholders that MUST be
substituted before parsing:

| Placeholder | Value |
| --- | --- |
| `{{NODE_SUBNET}}` | the node's IPv4 /24, e.g. `192.168.1.0/24` |
| `{{INSTALL_DISK}}` | the install disk, e.g. `/dev/vda` (v1 assumes `/dev/vda`) |

It pins the installer image to `factory.talos.dev/installer/<schematicId>:<talosVersion>`,
keeps the ZFS kernel module, disables flannel/kube-proxy, fixes host DNS and
inlines Cilium (`cilium/cilium.yaml`). Talos v1.14 `gen config` also emits a
stock `UnattendedInstallConfig`; the engine MUST strip it so the template's
`machine.install` stays authoritative (the Makefile `bootstrap-vm` target does
the same with a Python one-liner).

### 1.2 Values the engine must set

Apply `-f charts/naslos/values.yaml -f charts/naslos/values-installer.yaml`
plus overrides:

| Value | From |
| --- | --- |
| `domain` | the chosen local domain (`<name>.local`) |
| `sso.domains[0]` | same as `domain` |
| `shares.discovery.name` | the chosen short name (first label of `domain`) |
| `openldap.host` | `ldap.<domain>` (cert SAN + Authelia `server_name`) |
| `networkPolicy.nodeCIDR` | the node's /24 |
| `networkPolicy.ingressPluginsCIDR` | the LAN that reaches the UI |
| `networkPolicy.nfsClientCIDR` | the LAN allowed to mount NFS |
| `<comp>.image.repository` / `.tag` | the published image base for the pack version |

The chart generates `naslos-openldap`/`naslos-openldap-tls` itself (no
out-of-band step). The internal LDAP suffix (`openldap.baseDN`) is fixed at
`dc=naslos,dc=local` and must not follow the public domain: it is baked into the
mdb database on first boot.

## 2. Cluster objects the installer depends on

Namespaces: `naslos` (authenticated services), `naslos-privileged` (agent,
samba, nfs, **terminal**), `naslos-apps`, `naslos-apps-priv`.

| Object | Namespace | Used for |
| --- | --- | --- |
| `deploy/naslos-terminal` | `naslos-privileged` | in-cluster `curl` target for admin creation (has `curl`) |
| `statefulset/naslos-authelia`, pod `naslos-authelia-0` | `naslos` | deterministic exec target for TOTP generation |
| `secret/naslos-proxy` (key `secret`) | `naslos` | `X-Naslos-Proxy-Secret` value |
| `secret/naslos-tls` (`tls.crt`/`tls.key`) | `naslos` | optional OS trust-store import |
| `secret/naslos-openldap` (`admin-password`, `service-password`) | `naslos` | chart-generated LDAP credentials |
| `secret/naslos-openldap-tls` (`ca.crt`, `ldap.crt`, `ldap.key`) | `naslos` | chart-generated LDAP TLS |
| `deploy/naslos-api`, `secret/naslos-talosconfig` | `naslos` | health + node management |

`/var/lib/naslos/buddy-identity.json` is the Buddy Backup **KEK**: losing it
makes every stored backup unreadable, and `helm uninstall` would delete it (the
namespace is `helm.sh/resource-policy: keep`). The recovery README MUST say so.

## 3. First admin (`FR-INSTALL-06`)

Reuse the owner-gated API from inside the terminal pod. The exact path is in
[`identity-sso.md`](identity-sso.md#deployment--first-admin):

1. Read the proxy secret:
   `kubectl -n naslos get secret naslos-proxy -o jsonpath='{.data.secret}' | base64 -d`.
2. `remotecommand.Exec` into `deploy/naslos-terminal` (`naslos-privileged`) and
   run `curl` against the API Service with:
   - `Remote-User: <uid>`
   - `Remote-Groups: naslos_admins`
   - `X-Naslos-Proxy-Secret: <secret>`
   - `Content-Type: application/json`
   - body: `{"uid":"<uid>","displayName":"<uid>","email":"<uid>@<domain>","password":"<pw>","groups":["naslos_admins"]}`
3. Verify with `GET /api/users` (same headers) that the uid is present.

Source IP must be in `TRAEFIK_CIDR`; the terminal pod satisfies that. Password
policy: ≥8 chars, upper + lower + number (Authelia `password_policy`).

## 4. TOTP (`FR-INSTALL-07`)

The Authelia image is distroless; the `authelia` binary is on `PATH`, so exec a
binary directly (never a shell, never copy the config/DB out):

```
authelia storage user totp generate <uid> --issuer <domain>
```

Parse the printed `otpauth://totp/...?secret=<BASE32>` URI, render a QR, and
show the base32 secret in large type. Run it in pod `naslos-authelia-0`
(`naslos`). The storage encryption key env lives only in that pod.

**Spike (must be validated live before relying on it):** Authelia 4.38+ changed
second-factor registration; confirm a CLI-generated device lets the portal
accept a code without web enrolment. Fallback: drive portal enrolment and read
the elevated-session one-time code from `/config/notification.txt` (notifier is
`filesystem`).

## 5. Progress protocol

The engine (`naslos-install`, also embedded as a Tauri sidecar) writes
newline-delimited JSON to stdout; the shell renders the bar/log and cancels by
killing the child. One object per line, e.g.:

```json
{"step":"helm","status":"running","pct":55,"msg":"Installing chart"}
{"step":"done","status":"ok","pct":100,"msg":"https://naslos.local/authelia"}
{"error":{"step":"bootstrap","msg":"...","output":"..."}}
```

`step` identifiers, `status` and the terminal `done`/`error` shape are the
stable part; `msg` is human text.

## 6. ISO

For the pinned schematic (`4dd8e3a8b6203d3c14f049da8db4d3bb0d6d3e70c5e89dfcc1e709e81914f63c`)
and Talos `v1.14.1` (amd64 only):

```
https://factory.talos.dev/image/4dd8e3a8b6203d3c14f049da8db4d3bb0d6d3e70c5e89dfcc1e709e81914f63c/v1.14.1/metal-amd64.iso
```

The installer shows this link (and its checksum) and does not download or write
USB media.
