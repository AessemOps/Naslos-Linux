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
`release` GitHub Actions workflow on a semantic **`vX.Y.Z`** tag, and
embedded by the installer: `scripts/fetch-install-pack.sh` resolves the **newest
`vX.Y.Z` tag** of this repo, downloads the tarball, verifies its sha256 (and the
loader then verifies every member checksum), and extracts it into a gitignored
`installpack/` that is `go:embed`-ed. The Makefile derives the installer's
`ExpectedTalosVersion`/`ExpectedSchematicID` gate from that pack's
`metadata.json`. Non-semver tags (e.g. a moving `latest`) are ignored.

A released pack pins every chart image by digest from the **public** registry
`ghcr.io/aessemops/naslos-*` (FR-INSTALL-13): the `vX.Y.Z` workflow publishes the
images first, then rewrites the packed `values-installer.yaml` so each renders
`repository@sha256:…` (a digest wins over a tag). The in-tree
`values-installer.yaml` keeps tag placeholders, so a local `make install-pack`
(or `audit.sh`) is tag-only; only a released pack is digest-pinned. The
provisioned node has no `imagePullSecret`, so the packages must be **public**.

After attaching the pack, the `release` workflow sends a
`repository_dispatch` (`naslos-release`) to `AessemOps/Naslos-Installer`, so
publishing a pack rebuilds and republishes the installer against it (requires
the `INSTALLER_DISPATCH_TOKEN` secret in this repo).

`naslos-install-pack-<version>.tar.gz`:

```
naslos-install-pack-<version>/
  metadata.json
  charts/naslos/                       umbrella chart + charts/*.tgz subcharts
  charts/naslos/values-installer.yaml
  charts/cert-manager-webhook-ovh-<ver>.tgz   OVH DNS-01 solver (standalone chart)
  machine-config/naslos-installer.yaml.tmpl
  cilium/cilium.yaml
  manifests/local-path-v0.0.26.yaml
  manifests/cert-manager-crds.yaml     cert-manager CRDs (apply before it)
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
| `kubernetesVersion` | (additive) the Kubernetes version the installer generates and the web upgrader targets (e.g. `v1.37.0`) |
| `isoUrls` | `{"metal-amd64": "https://factory.talos.dev/image/<schematicId>/<talosVersion>/metal-amd64.iso"}` |
| `generatedAt` | UTC build timestamp |
| `checksums` | map of every other member path → sha256 |
| `images` | (additive; released packs) map of chart component → `ghcr.io/aessemops/naslos-<component>@sha256:…` for `api`, `agent`, `ui`, `samba`, `nfs`, `terminal`, `openldap`. The installer's parser ignores unknown fields, so its absence (a local pack) is fine |

The packed `values-installer.yaml` also carries a `release` block with the same
version facts plus the Kubernetes pin; the web upgrader reads it (§7).

The installer MUST verify every `checksums` entry before use and MUST refuse a
pack whose `talosVersion`/`schematicId` differ from the values it was built to
expect. `naslosVersion` is recorded in the recovery README.

### 1.1 Machine-config template

`machine-config/naslos-installer.yaml.tmpl` is a Talos machine-config **patch**
(one or more YAML documents) applied on top of the config generated from
`pkg/machinery`. It contains exactly three engine placeholders that MUST be
substituted before parsing:

| Placeholder | Value |
| --- | --- |
| `{{NODE_SUBNET}}` | the node's IPv4 /24, e.g. `192.168.1.0/24` |
| `{{INSTALL_DISK}}` | the install disk chosen by the operator, e.g. `/dev/vda` or `/dev/nvme0n1` |
| `{{INTERFACE}}` | the node's real NIC name, detected from the maintenance-mode Talos link/address resources (e.g. `eth0`, `ens3`, `enp1s0`) |

`{{INSTALL_DISK}}` MUST come from an explicit `--install-disk` argument; the
engine MUST NOT default to a device name. A non-dry-run install MUST also
require `--confirm-wipe=<same device>` so a typo cannot erase the wrong disk.
The value MUST be an absolute path under `/dev/`, contain no whitespace, `..`
or shell metacharacters, and MUST NOT be a partition when the node is known to
expose whole disks (the engine refuses a value that is not a whole device when
it can identify one). Choosing the disk is the operator's decision: the engine
does not infer it from the boot order.

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
| `<comp>.image.repository` / `.tag` | the published image base for the pack version. **A released pack** already pins `<comp>.image.digest` (or a full `openldap.image` reference): the engine MUST NOT override a digest-pinned reference with a repository/tag, and MUST leave `.image.digest`/`openldap.image` as the pack sets them (FR-INSTALL-13) |

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

### 2.1 cert-manager and the OVH webhook

The installer provisions the same certificate stack as `make install-vm`;
otherwise a fresh install can only serve the chart's generated self-signed
certificate and the Domains page cannot issue a publicly trusted one.

1. Apply `manifests/cert-manager-crds.yaml` server-side (force-conflicts). The
   CRDs must exist before any `Certificate`/`Issuer` is created.
2. Install `charts/naslos/charts/cert-manager-v1.18.2.tgz` as the release
   `cert-manager` in namespace `cert-manager` with `crds.enabled=false`.
3. Install `charts/cert-manager-webhook-ovh-<ver>.tgz` as the release
   `cert-manager-webhook-ovh` in the same namespace with
   `groupName=ovh.naslos.local` (must equal the API's OVH provider
   `webhook.groupName`), `certManager.namespace=cert-manager` and
   `certManager.serviceAccountName=cert-manager`.
4. Wait for both deployments before installing the Naslos release: the
   `values-installer.yaml` profile sets `ovhWebhook.enabled: true`, whose RBAC
   references the webhook ServiceAccount.

No operator credential is needed at install time. The webhook is
credential-less; OVH API credentials live in per-Domain Secrets written by the
UI. The step needs image-pull egress to `quay.io` (cert-manager) and Docker Hub
(`aureq/cert-manager-webhook-ovh`).

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
{"step":"totp","status":"running","pct":94,"msg":"Scan this code","data":{"otpauth":"otpauth://totp/...","secret":"BASE32"}}
{"step":"done","status":"ok","pct":100,"msg":"https://naslos.local/authelia","data":{"loginUrl":"https://naslos.local/authelia"}}
{"error":{"step":"bootstrap","msg":"...","output":"..."}}
```

`step` identifiers, `status` and the terminal `done`/`error` shape are the
stable part; `msg` is human text.

The optional `data` object on a step event carries the machine-readable values
the shell must not have to parse out of `msg`:

| Step | `data` keys |
| --- | --- |
| `totp` | `otpauth` (the full `otpauth://` URI), `secret` (base32) |
| `archive` | `path` (the recovery ZIP path) |
| `done` | `loginUrl` (the Authelia portal URL) |

The shell falls back to scanning `msg` when `data` is absent, so an older engine
still drives the wizard.

## 6. ISO

For the pinned schematic (`4dd8e3a8b6203d3c14f049da8db4d3bb0d6d3e70c5e89dfcc1e709e81914f63c`)
and Talos `v1.14.1` (amd64 only):

```
https://factory.talos.dev/image/4dd8e3a8b6203d3c14f049da8db4d3bb0d6d3e70c5e89dfcc1e709e81914f63c/v1.14.1/metal-amd64.iso
```

The installer shows this link (and its checksum) and does not download or write
USB media.


## 7. Web upgrades (`FR-UPD`)

Only a node installed from an **installer pack** is web-upgradable. A
`make install-vm` deployment has no `release` block: the API reports versions
and the UI disables Upgrade with a pointer to `make install-vm` (D2).

### 7.1 Release facts

`scripts/build-install-pack.sh` appends this block to the pack's
`values-installer.yaml` (after digest pinning), and adds `kubernetesVersion` to
`metadata.json`:

```yaml
release:
  profile: installer
  naslosVersion: 0.3.0
  chartVersion: 0.3.0
  talosVersion: v1.14.1
  schematicId: 4dd8…
  kubernetesVersion: v1.37.0
```

The chart renders them as `NASLOS_RELEASE_{PROFILE,VERSION,CHART_VERSION,TALOS_VERSION,SCHEMATIC_ID,KUBERNETES_VERSION}`.
The in-tree `values-installer.yaml` deliberately has no `release` block, so a
local `helm install` is not upgradable. The installer engine passes the pack's
`kubernetesVersion` explicitly to `generate.NewInput` (never the machinery
default).

### 7.2 Source and verification

The API resolves the newest **strict `vX.Y.Z`** GitHub release of
`AessemOps/Naslos-Linux` newer than the running version. `system.updates.source`
is a base URL: a GitHub API releases URL, a mirror serving `releases.json`, or a
path ending in `.json`. A mirror serves a GitHub-shaped array
(`{tag_name, html_url, body, published_at, assets:[{name,
browser_download_url}]}`) with strict `vX.Y.Z` tags and **absolute** asset URLs.
Each release must attach `naslos-install-pack-<X.Y.Z>.tar.gz` and its
`.sha256`; the API verifies the tarball sha256 and then every member checksum
from `metadata.json` before offering it. Nothing is cached unless it verifies.

The **stepping rule** offers the newest release whose `talosVersion` and
`kubernetesVersion` are each at most **one minor** ahead of the running
versions. If the newest is further ahead, the API offers the next eligible
release and reports why. A Naslos upgrade is blocked while the node's Talos or
Kubernetes differs from the *current* release's pins.

### 7.3 Operation Job

The API creates `Job naslos-system-op-<id>` in the release namespace:

| Field | Value |
| --- | --- |
| ServiceAccount | `naslos-upgrader` (ClusterRoleBinding `cluster-admin`) |
| image | `naslos`: the target pack's `images.api` digest. `talos`/`kubernetes`: the running API image |
| command | `["naslos-upgrade"]` (the binary is on `PATH` in the API image) |
| args | `--op <naslos\|talos\|kubernetes> --operation-id <id> --from <running> --version <target> --pack-url <url> --sha256 <approved> --namespace <ns>` |
| volumes | `naslos-talosconfig` at `/var/run/secrets/talos.dev`, the state PVC (`naslos-shares-config`) at `/var/lib/naslos`, an emptyDir at `/tmp` |
| retry | `backoffLimit` 3, `ttlSecondsAfterFinished` 3600; a Talos reboot kills the pod and the retry re-enters idempotently |

A `ValidatingAdmissionPolicy` + binding rejects any pod using the
`naslos-upgrader` ServiceAccount unless every container (and init container)
runs a `ghcr.io/aessemops/naslos-api@sha256:` image with
`command[0] == "naslos-upgrade"`. This is defence in depth: the account is
cluster-admin because the Job drives Talos, Helm and cert-manager.

The API's Role in the release namespace grants only: Jobs
get/list/create/delete, pods get/list, pods/log get, and `get` on the
`naslos-system-operation` ConfigMap.

### 7.4 Status and lock

`ConfigMap naslos-system-operation` (release namespace) has one `data.state`
JSON document:

```json
{"id":"…","op":"naslos","from":"0.3.0","to":"0.3.1","phase":"running",
 "steps":[{"name":"helm","status":"running","message":"…","startedAt":"…","finishedAt":""}],
 "message":"…","startedAt":"…","finishedAt":"","result":"","attempts":0}
```

`phase` is one of `pending`, `running`, `rebooting`, `succeeded`, `failed`. Any
active phase is the lock: `POST /api/system/operations` returns `409` while it
is present, and a second Job is never created. The ConfigMap lives in etcd, so
it survives API restarts and the Talos reboot.

### 7.5 Operations

**`naslos`** (run on the target image):

1. Re-download the pack, require the approved sha256, verify every member.
2. Preflight: profile `installer`, `to > from`; the API already checked the
   lock and that no app/Buddy job is running.
3. `talosctl etcd snapshot` to `/var/lib/naslos/backups/etcd-<ts>.db`; keep the
   last 3.
4. Converge prerequisites like the engine: apply local-path, apply the
   cert-manager CRDs server-side, `helm upgrade --install cert-manager
   --set crds.enabled=false` and the OVH webhook, then wait for their
   deployments.
5. Read the current release values, rebuild **only** the engine overrides
   (§7.6), and `helm upgrade naslos <pack chart> -f values.yaml -f
   values-installer.yaml -f overrides --reset-values --wait=false`. Then wait
   for OpenLDAP, Authelia, the API and the UI.
6. Verify every running workload image equals the pack's digest; on any
   failure `helm rollback naslos <previous revision>` and mark the operation
   failed.

**`talos`** (allowed only when the running Talos is older than the release pin):
preflight, etcd snapshot, then
`talosctl upgrade --image factory.talos.dev/installer/<schematic>:<version>
--wait=false --drain=false`. The pod dies with the reboot. On retry, the
operation re-reads `talosctl get version`: target reached → wait for the node
Ready and the agent DaemonSet rollout (the agent is the pool-health source),
patch `machine.install.image` to the new installer image, succeed; still the
old version after a retry → the A/B slot rolled back → fail with the reason.

**`kubernetes`** (allowed only when Talos equals the release pin and the
running Kubernetes is older): preflight, etcd snapshot,
`talosctl upgrade-k8s --to <version> --dry-run`, then the real upgrade, then
verify the API server version. There is no automatic rollback; see
`operations.md`.

### 7.6 Override keys

The upgrader rebuilds exactly the keys `Naslos-Installer/internal/helm/helm.go`
`Overrides` owns; everything else comes from the new pack (the release stores
fully merged values, including old image digests, so reusing them would keep
old images):

```
domain
sso.domains
shares.discovery.name
openldap.host
api.talosConfigSecret
networkPolicy.nodeCIDR
networkPolicy.ingressPluginsCIDR
networkPolicy.nfsClientCIDR
```

`rust/api/src/upgrade/overrides.rs` is unit-tested against a merged-values
fixture; keep this list and `helm.Overrides` identical.

### 7.7 API surface

| Route | Methods | Notes |
| --- | --- | --- |
| `/api/system/version` | GET | running Naslos/chart/Talos/Kubernetes, release pins, profile, `upgradable` |
| `/api/system/updates` | GET | cached check, candidate, stepping decision, notes URL |
| `/api/system/updates/check` | POST | run a check now (admin) |
| `/api/system/operations` | POST | `{op, version?}` → `202 {operationId}`; `409` lock/job conflict; `412` precondition |
| `/api/system/operations/current` | GET | status JSON plus the Job pod's log tail |
