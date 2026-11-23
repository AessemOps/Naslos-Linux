# Deployment

## Prerequisites

| Tool | Version |
| --- | --- |
| [talosctl](https://www.talos.dev/latest/talosctl/installation/) | 1.14+ |
| [kubectl](https://kubernetes.io/docs/tasks/tools/) | current |
| [helm](https://helm.sh/docs/intro/install/) | current |
| docker / podman | current |
| node | 20+ (build ui) |

### App install prerequisites

- **`make crds`** installs both the Traefik and the cert-manager CRDs. The app
  routing layer needs the Traefik CRDs; the SSL page is gated on the
  cert-manager CRDs.
- **Per-app isolation.** With `apps.isolation.enabled=true` (VM and installer
  overlays) the chart creates `apps.isolation.namespaces` baseline and
  `apps.isolation.privilegedNamespaces` privileged app namespaces, each with PSA,
  a quota (`apps.isolation.quota.*` for the baseline pool), a default-deny
  NetworkPolicy and the reduced Helm/API RoleBindings.
  New installs are assigned a free namespace and get one PV/PVC per selected
  dataset (`naslos-app-data` StorageClass). Legacy records stay in
  `naslos-apps` until reinstalled. See `docs/datasets-isolation.md`.
  The chart provisions a pool larger than the usable default: the API hands out
  only the first `apps.isolation.defaultLimit` slots (20) and the owner can
  raise or lower that limit up to the provisioned count (`32`) from
  **Apps → Installed**, no platform upgrade needed. The choice is persisted in
  `APPS_POOL_CONFIG` (`/var/lib/naslos/app-pool.json`) and survives upgrades.
  When every usable slot is taken, an install fails with
  `no free app namespace slot is available (limit N of M provisioned); raise the
  limit in Apps → Installed`. Growing the provisioned pool itself needs a chart
  change (raise `apps.isolation.namespaces`) and a platform upgrade.
- **Dataset exposure (legacy).** `apps.datasets.hostPath` (default `/var/mnt`) is
  mounted into legacy baseline apps. Fresh installs and the production VM
  overlay set `apps.datasets.enabled=false` and use only per-app claims, so the
  shared root is not rendered at all: VM datasets (`<pool>/vms`), the Buddy
  store and every other sibling under `/var/mnt` stay outside any app-visible
  volume. Installs that deliberately keep the legacy claim still get the chart
  guards: it refuses `/`, `/var`, `/etc`, `/home`, `/root`, `/usr`, `/system`,
  and refuses a Buddy store nested inside the root unless
  `apps.datasets.allowBuddyStore=true`. See `docs/datasets-isolation.md` for the
  per-app isolation design.
- **Catalog tracking and pinning.** By default the VM and installer overlays
  leave `apps.officialSource.commit` empty, so the API follows the configured
  channel branch (`main`) and a catalog refresh picks up new charts without a
  release. Set it to a full 40-character SHA when a reproducible/audited
  catalog is required: the API then checks out exactly that commit and fails
  the sync if `HEAD` differs (SEC-2026-D6). Source definitions loaded from
  `sources.json` can carry the same field.
- **App-install identity.** The chart creates a `naslos-helm` ServiceAccount
  and token Secret in the release namespace, bound to a namespaced Role in
  `naslos-apps` that excludes Traefik, cert-manager and RBAC writes. Helm uses
  that token (`HELM_SA_TOKEN_FILE`/`HELM_SA_CA_FILE`); routing/certs/exec keep
  using the API's own ServiceAccount (SEC-2026-D1/D4).
- **Version lockstep.** The installer release version MUST equal the Naslos
  version (`vX.Y.Z` ↔ `naslos-vX.Y.Z`); an installer-only patch is refused by
  the installer workflow. If the installer engine needs a fix, bump the Naslos
  patch, release the pack, and rebuild the installer at that version.
- **Installer dispatch token.** If the release run fails with `401 Bad
  credentials` at "Dispatch the installer rebuild", rotate
  `INSTALLER_DISPATCH_TOKEN` in the repository secrets, then run
  `scripts/dispatch-installer.sh vX.Y.Z` (or re-run the release workflow). The
  pack is already attached to the release; only the installer rebuild is
  missing.
- **cert-manager + the OVH webhook.** `make install-vm` installs both as
  prerequisites (`make cert-manager`, then `make cert-manager-webhook-ovh`) in
  the `cert-manager` namespace, outside the `naslos` default-deny policies. The
  desktop installer ships the same two charts plus the cert-manager CRDs in its
  install pack and provisions them during the install
  ([installer-contract.md](installer-contract.md) §2.1), so a fresh install can
  issue trusted certificates without a follow-up `helm` run. OVH
  is not a cert-manager built-in DNS-01 solver, so OVH certificates need the
  webhook, and the OVH API token must grant
  `GET/POST/PUT/DELETE /domain/zone/<zone>/*` (the `/status`, `/record` and
  `/refresh` subpaths). The chart's optional `certManager.enabled` subchart is
  an alternative for non-VM installs; the webhook is still required for OVH.
  Without them, apps still install and route; only ACME certificates are
  unavailable.
- **Wildcard DNS** for app subdomains: point `*.<domain>` at the node
  (router/dnsmasq/registrar). Without it, `<app>.<domain>` does not resolve.
- **A chart repository** with `apps/<name>/` entries (see
  [app-catalog.md](app-catalog.md)). Seed the official one with
  `apps.officialSource.url`; for SSH, create the deploy-key Secret first
  (see the Operational notes in [app-catalog.md](app-catalog.md)).
- **Pod egress to the git host.** The API clones repositories itself, so set
  `networkPolicy.gitEgress: true` (already on in `values-vm.yaml`) to allow
  outbound git (HTTPS 443 / SSH 22 / git:// 9418). The default target is
  `0.0.0.0/0`; narrow `networkPolicy.gitEgressCIDRs` for an air-gapped or
  egress-restricted network. `apps.officialSource.channels` maps channels to
  branches (e.g. `{Prod: main}` for a repository with only `main`).
- **Cilium node access (R5).** The `naslos-allow-host` policies grant
  node-local services only on 6443 (API server), 10250 (kubelet) and 50000
  (the upgrader's `talosctl`); the agent `:9090` and vmd `:9091` listeners are
  granted to the API pod alone by `naslos-api-host-egress`. The optional host
  firewall (`networkPolicy.cilium.hostFirewall`) follows the same split and
  additionally admits public 80/443 from `networkPolicy.publicIngressCIDR`;
  keep it off unless you have console access (see the template header).
- **Datasets for apps.** `apps.datasets` publishes the host datasets root
  (default `/var/mnt`) as a static RWX `naslos-datasets` PV/PVC that baseline
  apps mount, because PSA `baseline` forbids `hostPath` in `naslos-apps`.


## Images

| Image | Where it comes from |
| --- | --- |
| `ghcr.io/aessemops/naslos-api` | `.github/image-matrix.json` → `rust/api/Dockerfile` |
| `ghcr.io/aessemops/naslos-ui` | `ui/Dockerfile` |
| `ghcr.io/aessemops/naslos-agent` | `agent/Dockerfile.rust` |
| `ghcr.io/aessemops/naslos-openldap` | `openldap/image/Dockerfile` |

The GHCR packages are public, so a node needs no `imagePullSecret`. Pull a
specific build by its immutable `:sha-<commit>` tag (a dry-run release) or by
the digest a real release pins. `values-vm.yaml` carries the `sha-<commit>` tags
for the current node; a fresh image needs a new tag because the chart pulls with
`IfNotPresent`. There is no local/private registry in the install path.

### Pinning a digest (recommended for a real release)

A tag is mutable. The chart pulls with `IfNotPresent`, so `helm upgrade` after a
retag can keep running old code (NAS-022). Pin the exact image instead:

```bash
# Digests of the images a release built (or a dry run pushed as :sha-<commit>)
docker buildx imagetools inspect ghcr.io/aessemops/naslos-api:sha-<commit>
```

then set `<component>.image.digest=sha256:…` (or the release pack, which pins
every image by digest). `digest` accepts the value with or without the `sha256:`
prefix and takes precedence over `tag`; leave it empty to keep the tag behaviour.
**A pinned digest wins over a tag on the next upgrade too** — with
`--reuse-values` the stored digest is kept, so switching back to tags needs an
explicit `--set api.image.digest=` (empty). `openldap.image` is a full image
reference, so pin it there directly (`repo@sha256:…`).

R-035: the build inputs are pinned too. Every Dockerfile base image is
`tag@sha256:…` (Debian bookworm/trixie slim, Alpine 3.24, the Rust and Node
images, nginx-unprivileged), `bootstrap/local-path/local-path-storage.yaml`
pins both the provisioner and its busybox helper, and every GitHub Action is
referenced by commit SHA. `scripts/check-release-version.sh` walks
`values.yaml`, `values-installer.yaml` and `values-vm.yaml` and checks every
`image`/`provisionerImage` reference, including `@sha256:` and
`image.digest` forms; a naslos image tag must equal `appVersion` and an
unpinned non-naslos image fails the guard. The release guard also requires a
`vX.Y.Z` tag to point at a commit already on `master`. Residual: digest pins
are updated by hand (there is no automated digest-update workflow), so a
refresh means editing the Dockerfile digest and re-running the image build;
the release pack remains the authoritative per-release digest map.

## GHCR images + release tags

Every Naslos image lives in GHCR. A **release** publishes the same images
publicly (with immutable digests) so the desktop installer's node and the VM pull
them anonymously; a dry run publishes `:sha-<commit>` only:

- `.github/workflows/ci.yml` runs on every PR/push to `master`: the full
  `scripts/audit.sh` sweep (with `gitleaks` installed so the secret scan does not
  skip), a `supply-chain` job (`cargo audit`, `cargo deny`, `trivy fs`), a
  gitleaks scan, a no-push Docker build of all 9 images with a Trivy
  HIGH/CRITICAL gate and a CycloneDX SBOM per image, and a local install-pack
  build.
- `.github/workflows/release.yml` runs on a strict-semver **`vX.Y.Z`** tag:
  `guard` (the tag must equal `charts/naslos/Chart.yaml` `version`/`appVersion`
  and `ui/package.json` `version`; `scripts/check-release-version.sh` also
  requires every image tag in `values.yaml` and `values-installer.yaml` to match
  `appVersion`) → build + push all 9 images to
  `ghcr.io/aessemops/naslos-*` (linux/amd64) → build the install pack with every
  chart image **pinned by digest** (FR-INSTALL-13) → require green
  `audit`/`supply-chain`/`gitleaks` check runs for the commit, verify the pack
  checksum → attach the pack to the GitHub release → dispatch
  `AessemOps/Naslos-Installer` (`naslos-release`). `publish=true` is rejected on
  `workflow_dispatch`, so a dry run cannot mint a release tag; release runs are
  serialized globally.
- **Never push a throwaway `vX.Y.Z` tag**: `Naslos-Installer` resolves the
  newest strict-semver tag and would treat it as the release. Test with
  Actions → `release` → Run workflow (`version=0.1.0`, `publish=false`), which
  pushes only the immutable `:sha-<commit>` tag, builds the digest-pinned pack
  and uploads it as an artifact without touching the release, the mutable
  `:<version>` tag, or the installer dispatch. If a `vX.Y.Z` run fails before
  attaching the pack, delete the tag before retrying so the installer cannot
  resolve a packless tag.
- **Operator prerequisites** (not code): the `INSTALLER_DISPATCH_TOKEN` secret
  (fine-grained PAT with `Contents: read and write` on `AessemOps/Naslos-Installer`;
  without it the dispatch is skipped), and the 8 GHCR packages made **public**
  (one-time, per package → Settings → visibility). `GITHUB_TOKEN` with
  `packages: write` can publish but cannot change package visibility, so a
  `publish=true` release verifies anonymous pullability and fails if a package
  is still private. The image *tags* also live in GHCR as
  `naslos-<component>:<version>` and `:sha-<commit>`; the pack pins the immutable
  digests.

### Image footprint (2026-10-24 audit)

| Image | Base | Size | Why it cannot be smaller (yet) |
| --- | --- | --- | --- |
| `naslos-api` | Alpine | ~375 MB | ~246 MB is the three pinned CLIs (`talosctl` 118, `helm` 66, `kubectl` 62) the API shells out to, plus `git`/`openssh-client` for chart sources. A native Talos gRPC client would remove `talosctl` (the largest item) — a code change, not a base swap. |
| `naslos-agent` | scratch + static busybox | ~8 MB | Static musl binary; busybox supplies the `chroot` the agent uses to run host `zpool`/`zfs`. |
| `naslos-ui` | nginx-unprivileged alpine-slim | ~31 MB | Static assets; the slim variant drops the unused dynamic nginx modules. |
| `naslos-terminal` | Alpine | ~53 MB | The admin toolset (bash, e2fsprogs, util-linux, iproute2, iputils, bind-tools, curl, less, nano, jq); no unused `procps`/`tini`. |
| `naslos-samba` | Debian slim | ~264 MB | Samba + VFS modules; Debian is required because musl has no NSS module loading, so `libnss-extrausers` (the LDAP uid mirror) cannot work on Alpine. |
| `naslos-nfs` | Debian slim | ~153 MB | NFS-Ganesha + VFS FSAL (not packaged for Alpine). |
| `naslos-openldap` | Debian slim | ~213 MB | slapd + the Debian entrypoint paths/schema layout. |
| `naslos-buddy-receiver` | distroless static | ~17 MB | Static binary, no shell; the image is already minimal. |

The Debian images run `apt-get dist-upgrade` on purpose (security pocket) and
`--no-install-recommends`; packages nothing in the entrypoints/config uses are
not installed (e.g. `smbclient`, `avahi-utils`, `nfs-common`, `procps`).

## Make targets

| Target | What it does |
| --- | --- |
| `make all` | Build the agent and UI |
| `make agent` | `cargo build --release -p naslos-agent` (rust workspace) |
| `make ui` | `npm install && npm run build` (ui dir) |
| `make images` | Build all Naslos container images locally |
| `make push-images` | Push all Naslos container images to `REGISTRY` |
| `make image-digests` | Print each image's digest, for pinning with `*.image.digest` |
| `make bootstrap` | Bundle installer ISO/schematic tar |
| `make bootstrap-vm` | Generate the single-node VM Talos config |
| `make crds` | Apply Traefik CRDs from `helm show crds traefik/traefik` and the cert-manager CRDs |
| `make cert-manager` | Install cert-manager in the `cert-manager` namespace (CRDs come from `make crds`) |
| `make cert-manager-webhook-ovh` | Install the OVH DNS-01 webhook in `cert-manager` (group `ovh.naslos.local`) |
| `make install` | `helm dependency update && helm upgrade --install naslos charts/naslos -n naslos --create-namespace` (does **not** install cert-manager) |
| `make install-vm` | `make install` with the VM values **and its prerequisites** (`crds`, `cert-manager`, `cert-manager-webhook-ovh`, `local-path`, `naslos-talosconfig`, then a preflight that fails closed): Traefik on hostPort 80/443 with Authelia forwardAuth (see below). Requires `VM_IP=<node-ip>` (no default). This is the only posture |
| `make install-pack` | Build `dist/naslos-install-pack-<version>.tar.gz` for the desktop installer (FR-INSTALL; [installer-contract](installer-contract.md)) |
| `make uninstall` | `helm uninstall naslos -n naslos` |
| `make dev-cluster` | `talosctl cluster create --name naslos-dev` from the rendered schematic |

## Install flow

```bash
# 1. Image Factory schematic (ZFS extension)
make bootstrap

# 2. (Local dev) spin a cluster
make dev-cluster

# 3. Apply Traefik CRDs (required before first Helm install)
make crds

# 4. Install Naslos
make install

# 5. Access the UI
kubectl port-forward -n naslos svc/naslos-ui 8080:80
```

## Single-node VM deployment (e.g. 192.168.1.96)

For a bare-metal or virtual-machine Talos node without a cloud LoadBalancer,
use the VM-specific bootstrap and install targets.

### 1. Get images

Images are built and published by CI, not on the node or the operator's
workstation. Either consume a released `:X.Y.Z` image (digest-pinned by the
install pack) or run Actions → `release` → Run workflow with `publish=false`
to publish immutable `:sha-<commit>` tags for the current `master`, then put
those tags in `charts/naslos/values-vm.yaml` (all eight images from the same
commit). A new commit means new tags: the chart pulls with `IfNotPresent`.

The packages are public, so no registry login is needed on the node or the
build host. There is no local/private registry fallback.

### 2. Generate the VM Talos config

```bash
make bootstrap-vm
```

This writes:

- `bootstrap/vm/controlplane.yaml` — single-node control-plane machine config
  with the Naslos ZFS installer image and `192.168.1.96` as the cluster endpoint.
- `bootstrap/vm/talosconfig` — `talosctl` configuration.

### 2. Install Talos on the VM

Boot the VM from the Talos ISO (the Naslos ZFS schematic), then run:

```bash
# Apply the machine config to the still-insecure installer
export TALOSCONFIG=bootstrap/vm/talosconfig
talosctl apply-config --insecure --nodes 192.168.1.96 --file bootstrap/vm/controlplane.yaml

# Bootstrap etcd / Kubernetes
talosctl bootstrap --nodes 192.168.1.96 --endpoints 192.168.1.96

# Wait for the node to be ready
talosctl --nodes 192.168.1.96 health
```

### 3. Install Naslos on the VM

After bootstrapping, fetch Kubernetes credentials and deploy the cluster
storage. OpenLDAP (Secrets, StatefulSet, bootstrap Job, backup CronJob) is part
of the chart now, so there is no separate step for it:

```bash
# Make kubectl/helm talk to the new cluster
talosctl kubeconfig --nodes 192.168.1.96 --endpoints 192.168.1.96 -f ~/.kube/config

# `make install-vm` does this for you (targets `local-path` and
# `naslos-talosconfig`, both idempotent). To prepare a node by hand:
kubectl apply -f bootstrap/local-path/local-path-storage.yaml
kubectl wait --for=condition=ready pod -l app=local-path-provisioner -n local-path-storage --timeout=120s
kubectl label ns local-path-storage pod-security.kubernetes.io/enforce=privileged --overwrite
kubectl patch storageclass local-path \
  -p '{"metadata":{"annotations":{"storageclass.kubernetes.io/is-default-class":"true"}}}'
```

Then install Naslos. `VM_IP` is required: it both fills the endpoint in the
`naslos-talosconfig` Secret the API mounts and identifies the node:

```bash
make install-vm VM_IP=<node-ip>
```

A missing `VM_IP`, `local-path` StorageClass or `naslos-talosconfig` Secret
fails the preflight with the exact target to run; the Helm upgrade is never
attempted half-provisioned (R-068).

If you used a custom registry/tag, pass the overrides to Helm:

```bash
make install-vm VM_IP=<node-ip> HELM_FLAGS="\
  --set api.image.repository=docker.io/myuser/naslos-api \
  --set api.image.tag=latest \
  --set agent.image.repository=docker.io/myuser/naslos-agent \
  --set agent.image.tag=latest \
  --set ui.image.repository=docker.io/myuser/naslos-ui \
  --set ui.image.tag=latest"
```

### Web upgrader on a make-install node

The chart carries the full upgrader wiring (ServiceAccount, cluster-admin
binding, admission policy, Role). It renders only when `release.profile` is
set; a plain `make install-vm` leaves it off and the System page reports
versions with Upgrade disabled.

The production profile (`charts/naslos/values-vm.yaml`) sets
`release.profile: installer` plus the running Naslos/chart/Talos/Kubernetes
versions and schematic id, so the API polls releases and the System page can
apply them. Bump those facts with every deployed release; without them the
updater will not start (`Checker::from_env` ignores a non-installer profile).

The operation itself runs in a Job (`naslos-system-op-*`) as the
`naslos-upgrader` cluster-admin ServiceAccount. Its admission policy
(`system-upgrade.yaml`) pins the exact surface the binary needs: the published
API image, `command: ["naslos-upgrade"]`, the documented flags and env names,
no lifecycle/probes/envFrom, no init/ephemeral containers, no host namespaces,
no added capabilities, a non-root read-only container, and the Talos Secret /
state claim named by the chart values. Any other Job using that ServiceAccount
is denied.

**Pack handoff (why the upgrade does not re-download).** The update check
downloads the release pack, verifies its sha256 (and detached Ed25519 signature
when `SYSTEM_UPDATES_PACK_PUBKEY` is configured) and caches it on the state
volume at `/var/lib/naslos/updates/pack-<version>.tar.gz` plus `.sig`. The Job
mounts the same `naslos-shares-config` claim at `/var/lib/naslos` and reads
that already-verified file, re-checking the approved sha256 and signature
before extracting; a cached file that does not match fails the operation
instead of falling back. This keeps the upgrade working under the namespace
default-deny and across a Talos reboot retry. `naslos-system-op-egress` allows
HTTPS `443` for the Job as a fallback when the cache is absent (a profile
without `api.sharesConfig.enabled`, or an overridden `SYSTEM_UPDATES_CACHE`
outside `/var/lib/naslos`); in that case the pack is downloaded and verified in
the Job instead. Downloads never follow more than five redirects and every hop
must stay on the update source or a GitHub asset host; the body is streamed
under a 512 MiB cap after a `Content-Length` check.

Signature enforcement is fail-closed: a release that publishes `.sig` requires
`SYSTEM_UPDATES_PACK_PUBKEY`, and a configured key requires every release to be
signed. The chart does not set that variable, so a signed release channel needs
it added through a values overlay (the API reads it from its environment, and
the Job re-verifies the cached signature with the same key passed in
`--pack-pubkey`).

**Failure handling.** A failed Helm upgrade, a workload that does not become
ready (including the agent/samba/nfs DaemonSets), or a pack-digest mismatch on
the *running* pods all roll the release back to the previous Helm revision
(FR-UPD-06). A rollback failure is reported in the operation log with the
revision to restore manually.

### 4. Access the UI

The UI is reached through the proxy only:

```
https://naslos.local
```

### Automated deployment script

`scripts/deploy-vm.sh` is the historical fresh-node bootstrap helper (config
generation, apply, bootstrap, install). It now defaults to GHCR and does not
push images; for an existing node use `make install-vm` with
`charts/naslos/values-vm.yaml`, which is the supported path. Review it before
running:

```bash
chmod +x scripts/deploy-vm.sh
./scripts/deploy-vm.sh
```

Override the registry and/or secret as needed:

```bash
REGISTRY=docker.io/myuser REGISTRY_HTTP_SECRET=mypassword IMAGE_TAG=latest \
  ./scripts/deploy-vm.sh
```

> Grafana was **removed** on 2026-09-19 (AUDIT-H4: it shipped a committed default
> admin password and was unused). The LDAP credentials are no longer committed:
> the chart generates the admin password and the LDAP service credential on
> first install and preserves them across upgrades (`naslos-openldap` Secret,
> AUDIT-H1/NAS-010).

### Files added for the VM

| Path | Purpose |
| --- | --- |
| `bootstrap/vm/naslos-vm.yaml` | Machine-config patch for the VM installer/network/cluster |
| `bootstrap/vm/controlplane.yaml` | Generated by `make bootstrap-vm` |
| `bootstrap/vm/talosconfig` | Generated by `make bootstrap-vm` |
| `charts/naslos/values.yaml` | Helm values: ClusterIP Traefik, chart-generated OpenLDAP Secrets, auth/ingress defaults (ZFS LocalPV and the bundled ntfy chart were removed). The chart installs **no** storage provisioner: `local-path` comes from `deploy-vm.sh`/the installer pack |
| `charts/naslos/values-vm.yaml` | Helm override for the VM: `naslos.local`, Traefik hostPort 80/443, ingress + Authelia on, registry image tags |
| `charts/naslos/values-installer.yaml` | Installer profile: published image repository base, no private registry (FR-INSTALL). Its in-tree image tags must equal `Chart.yaml` `appVersion`, as must those in `values.yaml`; `scripts/check-release-version.sh` enforces both (a released pack then rewrites the installer tags to digests) |
| `bootstrap/installer/naslos-installer.yaml.tmpl` | Parameterised machine-config patch shipped in the install pack |
| `scripts/deploy-vm.sh` | One-shot VM deployment script |
| `scripts/build-install-pack.sh` | Builds `dist/naslos-install-pack-<version>.tar.gz` |
| `rust/api/Dockerfile`, `rust/api/Dockerfile.receiver`, `agent/Dockerfile.rust`, `ui/Dockerfile` | Container image builds for Naslos components |

### Fixes baked into the deployment (learned the hard way — do not remove)

| Fix | Where | Why |
| --- | --- | --- |
| ZFS installer `factory.talos.dev/installer/4dd8e3a8…f63c:v1.14.6` in `machine.install` | `bootstrap/vm/naslos-vm.yaml` | This is the Image Factory image for `bootstrap/schematic/naslos.yaml` (the official `siderolabs/zfs` extension). Pinned to v1.14.6 (the current release target; the running node is still v1.14.5 until it is upgraded). Storage for Kubernetes PVCs is local-path; ZFS pools are agent-managed. |
| `taints: $patch: delete` in `KubeNodeConfig` | `bootstrap/vm/naslos-vm.yaml` | Talos v1.14 default-applies `control-plane:NoSchedule` on a single-node cluster; with the taint, every workload without a toleration stays Pending. |
| No VIP on `eth0` (plain DHCP + reservation) | `bootstrap/vm/naslos-vm.yaml` | VIP == DHCP address fights kubelet node-IP selection (`no suitable node IP found`). |
| `UnattendedInstallConfig` strip after `gen config` | `Makefile` (`bootstrap-vm`) | v1.14 `gen config` emits a stock UIC doc that is mutually exclusive with `machine.install` — apply fails with "UnattendedInstallConfig config is incompatible with v1alpha1 config". |
| `pod-security.kubernetes.io/enforce=privileged` on `local-path-storage`, `naslos` **and** `naslos-privileged` | `scripts/deploy-vm.sh` | The local-path helper pod uses hostPath, and the API mounts hostPath volumes, so baseline PSA rejects them (`violates PodSecurity "baseline:latest": hostPath volumes`). The helper pod runs in the provisioner's namespace and the hostNetwork/privileged workloads live in `naslos-privileged`, so all three need the label. |
| `local-path` patched as default StorageClass | `scripts/deploy-vm.sh` | Chart PVCs don't set `storageClassName`; without a default class they never bind. |
| Helm ownership labels/annotations on the pre-created `naslos` namespace | `scripts/deploy-vm.sh`, `Makefile` (`--take-ownership`) | `helm --create-namespace` fails with "invalid ownership metadata" on an existing namespace; the namespaces and the old kubectl-created OpenLDAP Secrets are adopted into the release with `--take-ownership`. |
| Chart-generated OpenLDAP Secrets (lookup-guarded) | `charts/naslos/templates/openldap-secrets.yaml` | Previously `openldap/generate-secrets.sh` + standalone manifests outside Helm; the chart now generates them on first install and re-emits the existing bytes on upgrade so OpenLDAP is never re-keyed. |
| Talos admin `talosconfig` mounted into the API pod | `charts/naslos/templates/api-deployment.yaml` (`api.talosConfigSecret`) | The API resolves its endpoints from the talosconfig context; the chart does not set `TALOS_ENDPOINTS` (`TALOS_ENDPOINTS` is only an optional override). Without the mount, node management and metrics are disabled. |
| Agent degraded mode when ZFS is missing | `rust/agent/src/main.rs` + `rust/agent/src/server/` | The agent used to `log.Fatalf` without `/dev/zfs`; now it starts and ZFS endpoints return 503 instead of crash-looping. |
| No `olcTLSCipherSuite` in the slapd config LDIF | `openldap/image/entrypoint.sh` | Debian's OpenLDAP is built against GnuTLS, which rejects OpenSSL cipher strings (`HIGH:!aNULL:!MD5`) — `slapadd -n 0` fails on `cn=config` with an opaque error. |
| LDIF continuation lines with a single leading space | `openldap/image/entrypoint.sh` | Four-space continuations corrupt `olcAccess` values. |
| `--skip-crds` on Helm install | `Makefile` (`install`, `install-vm`) | Avoids CRD ownership conflicts with previously kubectl-applied CRDs. |

## Day-2 network changes (`FR-NET`)

The install path is deliberately DHCP with an auto hostname
(`bootstrap/vm/naslos-vm.yaml`,
`bootstrap/installer/naslos-installer.yaml.tmpl` and the installer wizard).
Static addressing, custom DNS and MTU are day-2 operations from
**Settings → Network** (`/settings/network`); they are not installer fields.

Use the page to:

- switch one physical link between DHCP and static addressing (1–4 CIDR
  addresses, optional gateway);
- set an MTU (576–9216; below 1280 needs an explicit confirmation);
- replace the resolver list or search domains, or return them to DHCP.

The install-time `hostDNS` setting is preserved by every resolver patch and the
UI shows it read-only. The hostname is read-only too: renaming the node breaks
the `kubernetes.io/hostname` node affinity on every local-path volume, so a
rename is not offered.

### Guard windows and re-pointing

A change runs `talosctl patch machineconfig --mode=try`. Talos applies it
immediately and reverts after 60 seconds unless another configuration update
lands. The API verifies the new state inside that window, confirms it with a
certSANs update, and then keeps a five-minute record so the operator can revert.
If the API pod restarts mid-change, the watchdog resumes or reverts the saved
record instead of leaving a pending change that blocks every later edit.

When an address changes, the browser loses the page and `https://<name>` only
works again once DNS (or `/etc/hosts`) points at the new address. Re-point it
from a machine still on the LAN; the Traefik hostPorts follow the node IP
automatically.

### Subnet and CNI caveats

- A static address outside the kubelet's current subnet is added to
  `KubeNodeConfig.nodeIP.validSubnets` in the same change. Without that the
  kubelet can pick the wrong node IP after the next restart.
- After an MTU change is confirmed the API rolls the Cilium DaemonSet, because
  the CNI detects the device MTU at agent start. Cilium's `devices: ""` means it
  auto-detects the interface.
- Resolver changes take effect through the host resolver (`hostDNS.enabled`), so
  no chart restart is needed.

### Recovering by hand

If the page is unreachable and the guard did not restore the node, follow
[operations.md → Network recovery](operations.md#network-recovery). The saved
snapshot lives in `NETWORK_STATE` (`/var/lib/naslos/network.json` on the state
PVC).

## The posture: Authelia + Traefik (`values-vm.yaml`)

This is the only posture. The instance is reachable exclusively through the
authenticating proxy; there is no NodePort, no `auth.disabled` and no other
unauthenticated listener:

```
https://naslos.local
  -> Traefik (hostPort 80/443 on the node)
     -> /authelia            -> Authelia portal (no forwardAuth)
     -> /api                 -> naslos-api   (forwardAuth + proxy-identity)
     -> everything else      -> naslos-ui    (forwardAuth)
```

- **Hostname** `naslos.local`: already resolvable through the mDNS record the
  Samba container advertises (`host-name=naslos`). Add a router/local-DNS A
  record `naslos.local -> <VM IP>` for clients without mDNS (Windows).
- **TLS**: the chart generates a self-signed certificate for the `domain` value
  on first install and reuses it across upgrades
  (`templates/tls-secret.yaml`, `lookup`-guarded). Import the `naslos-tls`
  Secret's `tls.crt` on a client to remove the browser warning, or point
  `ingress.tls.existingSecret` at a Secret from a real CA (keys `tls.crt` /
  `tls.key`).
- **2FA**: Authelia requires `two_factor` for `group:naslos_admins` on every path
  (users, groups, apps, volumes, datasets, disks, shares, pods, namespaces, ws,
  the owner buddy routes and the SPA itself). Enroll TOTP/WebAuthn at the first
  portal login.
- **No unauthenticated listener**: the UI NodePort and the `auth.disabled` /
  `AUTH_DISABLED` / `AGENT_AUTH_DISABLED` bypasses were removed. The API and the
  agent refuse to start without their credential.
- **Traefik dashboard**: `https://naslos.local/traefik/dashboard/`, linked from
  the sidebar for administrators only. The API is not exposed on any port
  (`api.insecure` off) and Authelia denies non-admin accounts on `/traefik`.

### Traefik RBAC (R-011)

The chart runs Traefik with `rbac.namespaced: true`, which replaces the
upstream ClusterRole/ClusterRoleBinding with a Role/RoleBinding in the release
namespace and in every namespace listed under
`providers.kubernetesCRD.namespaces`. The production and installer overlays
list the shared app namespaces (`naslos-apps`, `naslos-apps-priv`) plus the
fixed isolation pool (`naslos-app-1..32`, `naslos-app-p1..p4`), so an app route
or its copied TLS Secret is readable where it is created. A compromised
Traefik pod can therefore read Secrets only in those platform namespaces, not
cluster-wide; the residual is that it can read *all* Secrets in them
(including the per-domain TLS copies and the app catalog credentials), because
`rbac.secretResourceNames` cannot express Traefik's per-domain TLS Secret names.
When you change `apps.isolation.namespaces` or `privilegedNamespaces`, update
the overlays' watched list in the same change. Keep
`apps.officialSource.credentialsSecret` empty on public catalogs, as the chart
does.

### First install / creating the operator

The chart does not seed a human account, and `/api/users` is an owner route, so
a bare `kubectl port-forward` POST now returns `401`: the request must carry the
`X-Naslos-Proxy-Secret` (Secret `naslos-proxy`) plus `Remote-User`/
`Remote-Groups`, from a source the API's NetworkPolicy allows. The working
options (an in-cluster curl from the Traefik/UI/`naslos-privileged` pods, or a
direct `ldapadd`) are in
[identity-sso.md](identity-sso.md#deployment--first-admin).

```bash
make install-vm VM_IP=<node-ip>
```

Verify, in order: `kubectl -n naslos get ingressroute` shows `naslos-ui`,
`naslos-api`, `naslos-authelia-portal` and `naslos-redirect`;
`kubectl -n naslos get svc` has **no** NodePort; `curl -sk
https://naslos.local/api/health` is 200; `curl -skI
https://naslos.local/api/users` redirects to `/authelia` (not 200); the browser
reaches `/authelia`, logs in, registers 2FA, and lands on the dashboard; the
Terminal page works (it proves `/api` goes straight to the API);
`http://<vm-ip>:30080` refuses.

### Recovery

There is no unauthenticated fallback. If the proxy or Authelia misbehaves, use
cluster access:

```bash
helm -n naslos rollback naslos          # previous release
# or, after a bad commit:
git revert <sha> && make install-vm VM_IP=<node-ip>
```

Pools, datasets and the state PVC (shares, buddy identity, notifications) are
untouched by a rollback.

**Operator locked out (lost the TOTP device, Authelia DB corruption, portal
loop).** In order, least destructive first:

1. **Another factor still works?** Log in with a registered WebAuthn device, or
   the recovery/one-time codes if they were saved. The elevated-session one-time
   code is written to the Authelia volume and can be read with
   `kubectl -n naslos exec statefulset/naslos-authelia -- cat /config/notification.txt`.
2. **Session/identity checks.** Confirm the Authelia pod is running and can bind
   LDAP: its logs show `LDAP Result Code 49` when the bind password and the
   `naslos-openldap` Secret's `service-password` disagree. Fix by aligning the
   Secret (never by committing a literal).
3. **Reset the second factor from the DB.** Stop Authelia
   (`kubectl -n naslos scale statefulset/naslos-authelia --replicas=0`), then either
   restore `/config/db.sqlite3` from a backup on the PVC, or delete the affected
   rows in `totp_devices` / `webauthn_devices` with `sqlite3` in a throwaway pod
   that mounts the same PVC. Scale back up and enrol again at first login.
4. **Last resort: `helm -n naslos rollback naslos`** (or `git revert` + `make
   install-vm`). The Authelia PVC, pools and the shares/buddy/notifications PVCs
   are not part of the rollback, so nothing is lost; the proxy posture is simply
   restored to the last working revision.

Never re-introduce a bypass to recover: the removed NodePort / `AUTH_DISABLED`
paths are gone for a reason, and cluster access is always available.

### Running the E2E suite

The Playwright suite authenticates like a person: a `setup` project logs into the
Authelia portal once (password + a TOTP generated from the shared secret) and
every test reuses that session through `storageState`. Credentials are
mandatory; a target that does not challenge is an error, not a fallback.

```bash
cd ui
# credentials, once (never committed - ui/.env.playwright.local is gitignored)
printf 'NASLOS_ADMIN_USER=admin\nNASLOS_ADMIN_PASSWORD=%s\nNASLOS_TOTP_SECRET=%s\n' \
  '<password>' '<base32 secret>' > .env.playwright.local
chmod 600 .env.playwright.local

PLAYWRIGHT_BASE_URL=https://naslos.local \
NASLOS_RECEIVER_URL=http://naslos-api.naslos.svc.cluster.local:8080 \
  npx playwright test
```

- `NASLOS_TOTP_SECRET` is the base32 secret behind the enrolment QR; without it
  the suite cannot authenticate. `NASLOS_TOTP_CODE` works for a single run only.
  Authelia refuses a code it has already accepted, so a second run inside the
  same 30 s window waits for the next one and retries.
- `NASLOS_RECEIVER_URL` matters for the two self-send tests: a page-driven send
  targets the browser origin, and the API pod cannot resolve `naslos.local`
  (mDNS is not in cluster DNS), so point it at the in-cluster API. On a network
  with a real DNS record for the appliance it can be omitted.
- The suite **mutates the live instance**: it creates and deletes users, groups,
  shares and datasets, writes and removes buddy schedules, and performs real
  self-sends. Run it on a maintenance window, not on a system you cannot afford
  to see churned.
- The interactive terminal tests run here: `/api` goes straight from Traefik to
  the API, so the authenticated session reaches it.

## Helm values walkthrough (`charts/naslos/values.yaml`)

| Key | Default | Notes |
| --- | --- | --- |
| `api.*` | enabled, 1 replica, port 8080 | ClusterIP service |
| `ui.*` | enabled, 1 replica, port 80 | ClusterIP service |
| `agent.*` | privileged, host mounts | DaemonSet per node; `agent.smartctl.*` bundles the static SMART reader and mounts `/sys` read-only |
| `diskHealth.*` | enabled, 55 °C, 80 % | per-disk SMART/ZFS notification thresholds |
| `shares.*` | SMB/NFS on, AFP off | see [shares.md](shares.md) |
| `traefik.*` | web :80, websecure :443 | LoadBalancer service, dashboard API on with `basePath: /traefik`; `values-vm` adds `ports.*.hostPort` |
| `authelia.*` | domain `naslos.local` | config map; LDAP backend in template; portal at `/authelia` when ingress is on |
| `ingress.*` | disabled | Traefik IngressRoutes + middlewares; `ingress.tls` (secretName/existingSecret) |
| `auth.*` | generated shared secret | owner-route gate (NAS-001); no `disabled` switch exists |
| `ntfy.server.url` | empty ⇒ public `ntfy.sh` | no bundled ntfy chart; see [notifications.md](notifications.md) |
| `storage.*` | localPath only | see [storage-zfs.md](storage-zfs.md) |
| `openldap.*` | bind DN + image | the bind/admin passwords are generated by the chart into the `naslos-openldap` Secret (lookup-guarded, preserved across upgrades); nothing is committed |
| `workloadNamespace` | `naslos-privileged` | namespace for the hostNetwork/privileged workloads (AUDIT-M6) |
| `namespace` | (removed) | every template uses `.Release.Namespace`, so `helm -n <ns>` decides it (AUDIT-L9) |

## ZFS ARC

> **Not capped, and not planned (2026-10-24).** The runtime knob works
> (`echo <bytes> > /sys/module/zfs/parameters/zfs_arc_max` drops `c_max`
> immediately, verified live), but no boot-persistent Talos route reaches it on
> this platform:
>
> - `machine.kernel.modules[].parameters: [zfs_arc_max=<bytes>]` is ignored: the
>   `siderolabs/zfs` extension loads the module itself, so Talos never passes the
>   parameter (`zfs_arc_max` stayed `0` across two reboots on Talos v1.14.1).
> - `machine.install.extraKernelArgs` is unsupported with SDBoot.
> - A `machine.files` `/etc/modprobe.d/zfs.conf` drop-in was ignored too and
>   coincided with a CRI boot failure (reverted).
>
> A privileged agent initContainer could write the host sysfs value on every
> boot, but that trade-off was rejected, so the item is closed. The default
> `c_max` is ~80 % of RAM (~4.76 GiB of the 5.76 GiB node). `zfs.arcMax` is not
> in the chart because nothing consumed it.

## Secrets to rotate before production

- LDAP credentials live only in the chart-generated `naslos-openldap` Secret
  (`admin-password`, `service-password`) and are no longer in values
  (AUDIT-H1 fixed). To rotate the service password: patch the Secret, then
  re-run the bootstrap Job (delete the completed
  `job/naslos-openldap-bootstrap` and re-run `make install-vm`; it is a
  post-install/post-upgrade hook, so the upgrade recreates it) so the directory
  entry gets the new SSHA, then the pods roll.
- The OpenLDAP **admin** password is generated randomly by the chart on first
  install (NAS-010, closed). To choose it explicitly set `openldap.adminPassword`
  for one install, or patch the Secret afterwards; the StatefulSet's initial
  database bootstrap uses the Secret value at first boot.
- `authelia-config` `jwt_secret` (generated once into the `naslos-authelia-jwt`
  Secret since 2026-09-19, so it survives upgrades; rotate if it shows up in
  git/diffs)
- Internal CA (`naslos-openldap-tls` secret) — replace with a real CA if you
  want browser-trusted HTTPS.
- `naslos-tls` (the ingress certificate) is chart-generated and self-signed for
  the `domain` value; trust it per client, or set `ingress.tls.existingSecret`
  to a real certificate. To force renewal, delete the Secret and re-run the
  upgrade.

## Upgrades

- **Helm**: `make install` re-runs `helm upgrade --install`, applying new chart
  values and templates.
- **Talos**: `talosctl upgrade`; the ZFS extension comes from the schematic
  (see [bootstrap.md](bootstrap.md#upgrades)).
- **Images**: bump `*.image.tag` in `values.yaml` and re-run `make install`.

## Desktop installer (install pack)

The desktop installer (`Naslos-Installer`, a separate repo) provisions a
freshly-booted Talos node end to end. Naslos-Linux only supplies the pack and
the stable interfaces ([installer-contract.md](installer-contract.md); spec
`FR-INSTALL`):

```bash
make install-pack            # dist/naslos-install-pack-0.1.0.tar.gz (+ .sha256)
```

The pack bundles the umbrella chart (with vendored subcharts),
`values-installer.yaml`, the parameterised machine-config template, Cilium, the
pinned local-path manifest and the schematic, plus a `metadata.json` that pins
`talosVersion`, `schematicId`, the amd64 ISO URL and a sha256 per member. The
`release` GitHub Actions workflow attaches it, and the checksum file, to a
semantic `vX.Y.Z` release, then dispatches `AessemOps/Naslos-Installer` so the
installer rebuilds against it. The installer resolves the **newest `vX.Y.Z`
tag** (it ignores non-semver tags such as `latest`) and derives its Talos /
schematic gate from the pack's `metadata.json`.

A **released** pack also pins each chart image by digest to
`ghcr.io/aessemops/naslos-*` (FR-INSTALL-13): the tag workflow publishes the
images first, then `scripts/pin-installer-values.py` rewrites the packed
`values-installer.yaml` and `metadata.json.images` records the exact
references. A digest wins over a tag (VER-2), and the pack build fails closed if
any of the 9 chart images is missing from the digest map. A local
`make install-pack` has no digest map and keeps the tag placeholders. See
"GHCR images + release tags" above.

`values-installer.yaml` is the installer profile: it parameterises the image
repository base and **must not** reference the private VM registry. The engine
overrides `domain`, `sso.domains`, `shares.discovery.name`, `openldap.host` and
the three LAN `networkPolicy` CIDRs (`nodeCIDR`, `ingressPluginsCIDR`,
`nfsClientCIDR`) at install time; it must **not** override a digest-pinned image
reference. `networkPolicy.publicIngressCIDR` comes from the packed
`values-installer.yaml` (`0.0.0.0/0`) to allow public app domains through
Traefik.

## Uninstall

```bash
make uninstall
# NOTE: this does NOT delete ZFS pools or PVCs. Clean those up explicitly.
```