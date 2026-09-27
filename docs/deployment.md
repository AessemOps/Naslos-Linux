# Deployment

## Prerequisites

| Tool | Version |
| --- | --- |
| [talosctl](https://www.talos.dev/latest/talosctl/installation/) | 1.14+ |
| [kubectl](https://kubernetes.io/docs/tasks/tools/) | current |
| [helm](https://helm.sh/docs/intro/install/) | current |
| docker / podman | current |
| go | 1.26.5+ (build api/agent; see `api/go.mod`) |
| node | 20+ (build ui) |

### App install prerequisites

- **`make crds`** installs both the Traefik and the cert-manager CRDs. The app
  routing layer needs the Traefik CRDs; the SSL page is gated on the
  cert-manager CRDs.
- **cert-manager + the OVH webhook.** `make install-vm` installs both as
  prerequisites (`make cert-manager`, then `make cert-manager-webhook-ovh`) in
  the `cert-manager` namespace, outside the `naslos` default-deny policies. OVH
  is not a cert-manager built-in DNS-01 solver, so OVH certificates need the
  webhook, and the OVH API token must grant
  `GET/POST/PUT/DELETE /domain/zone/<zone>/*` (the `/status`, `/record` and
  `/refresh` subpaths). On a non-VM install the chart's optional
  `certManager.enabled` subchart can install the controller, but the OVH webhook
  is still required for OVH. Without either, apps still install and route; only
  ACME certificates are unavailable.
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
- **Datasets for apps.** `apps.datasets` publishes the host datasets root
  (default `/var/mnt`) as a static RWX `naslos-datasets` PV/PVC that baseline
  apps mount, because PSA `baseline` forbids `hostPath` in `naslos-apps`.


## Images

| Image | Where it comes from |
| --- | --- |
| `192.168.1.2:30095/naslos-api` | `make api` + container build |
| `192.168.1.2:30095/naslos-ui` | `make ui` + container build |
| `192.168.1.2:30095/naslos-agent` | `make agent` + container build |
| `192.168.1.2:30095/naslos-openldap` | `openldap/image/Dockerfile` |

### Pinning a digest (recommended for a real release)

A tag is mutable. The registry reuses `0.1.0` and the chart pulls with
`IfNotPresent`, so `helm upgrade` after a retag can keep running old code
(NAS-022). Pin the exact image instead:

```bash
make image-digests                      # prints "image -> repo@sha256:…" per component
helm upgrade naslos charts/naslos -n naslos --reuse-values \
  --set api.image.digest=sha256:… --set ui.image.digest=sha256:…
```

`<component>.image.digest` accepts the value with or without the `sha256:`
prefix and takes precedence over `tag`; leave it empty to keep the tag behaviour
(and therefore the "always retag" rule). **A pinned digest wins over a tag on the
next upgrade too** — with `--reuse-values` the stored digest is kept, so switching
back to tags needs an explicit `--set api.image.digest=` (empty). `openldap.image` is a full image
reference, so pin it there directly (`repo@sha256:…`).

## Make targets

| Target | What it does |
| --- | --- |
| `make all` | Build api, agent, ui binaries |
| `make api` | `go build -o ../bin/naslos-api ./cmd` (api dir) |
| `make agent` | `go build -o ../bin/naslos-agent ./cmd` (agent dir) |
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
| `make install-vm` | `make install` with the VM values **and its prerequisites** (`crds`, `cert-manager`, `cert-manager-webhook-ovh`): Traefik on hostPort 80/443 with Authelia forwardAuth (see below). This is the only posture |
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

### 1. Generate/build (one-time)

Build the Naslos container images and push them to a registry reachable by the VM.
The default registry is `192.168.1.2:30095`; override it if you are using a different
registry.

This is a private plain-HTTP registry; the VM bootstrap patch
(`bootstrap/vm/naslos-vm.yaml`) configures Talos to use it as an insecure
mirror with the registry password `secret` (either an empty username or the
placeholder `_`, depending on what your registry's basic-auth accepts —
`scripts/deploy-vm.sh` tries both when pushing).

On the build host, Docker must also be told the registry is insecure.
`scripts/deploy-vm.sh` tries to merge it into `/etc/docker/daemon.json` as a
best-effort step: it uses `sudo` when you are not root, skips the write with a
warning when neither root nor `sudo`/`jq` is available, and never aborts the
deploy on this step. If you push manually, make sure `192.168.1.2:30095`
is listed under `insecure-registries` and that the Docker daemon was
restarted afterwards (`sudo systemctl restart docker`).

```bash
# Build locally
make images

# Push (the registry password is `secret`;
# `deploy-vm.sh` logs in automatically with the empty/`_` username fallback)
make push-images

# Or use a custom registry/tag
make push-images REGISTRY=docker.io/myuser IMAGE_TAG=latest
```

If your registry requires a different HTTP password, set `REGISTRY_HTTP_SECRET`
before running `scripts/deploy-vm.sh`:

```bash
REGISTRY_HTTP_SECRET=mysecret ./scripts/deploy-vm.sh
```

If you prefer a private registry other than the default, set `REGISTRY` and
also pass the image overrides when installing (see step 3).

### 2. Generate the VM Talos config

```bash
make bootstrap-vm
```

This writes:

- `bootstrap/vm/controlplane.yaml` — single-node control-plane machine config
  with the Naslos ZFS installer image and `192.168.1.96` as the cluster endpoint.
- `bootstrap/vm/talosconfig` — `talosctl` configuration.

### 2. Install Talos on the VM

Boot the VM from the Naslos installer image (ISO/USB/PXE), then run:

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

# Deploy local-path-provisioner (single-node VM, no ZFS pool required).
# Pinned in-repo; the installer pack ships the same file.
kubectl apply -f bootstrap/local-path/local-path-storage.yaml
kubectl wait --for=condition=ready pod -l app=local-path-provisioner -n local-path-storage --timeout=120s
kubectl label ns local-path-storage pod-security.kubernetes.io/enforce=privileged --overwrite
kubectl patch storageclass local-path \
  -p '{"metadata":{"annotations":{"storageclass.kubernetes.io/is-default-class":"true"}}}'
```

Then install the rest of Naslos:

```bash
make install-vm
```

If you used a custom registry/tag, pass the overrides to Helm:

```bash
make install-vm HELM_FLAGS="\
  --set api.image.repository=docker.io/myuser/naslos-api \
  --set api.image.tag=latest \
  --set agent.image.repository=docker.io/myuser/naslos-agent \
  --set agent.image.tag=latest \
  --set ui.image.repository=docker.io/myuser/naslos-ui \
  --set ui.image.tag=latest"
```

### 4. Access the UI

The UI is reached through the proxy only:

```
https://naslos.local
```

### Automated deployment script

For convenience, `scripts/deploy-vm.sh` runs the full flow (build images,
generate config, apply, bootstrap, install). It defaults to the private HTTP
registry `192.168.1.2:30095` and **requires** `REGISTRY_HTTP_SECRET` (there is no
default). Review it before running:

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
| `charts/naslos/values.yaml` | Helm values: ClusterIP Traefik, local-path storage, chart-generated OpenLDAP Secrets, auth/ingress defaults (ZFS LocalPV and the bundled ntfy chart were removed) |
| `charts/naslos/values-vm.yaml` | Helm override for the VM: `naslos.local`, Traefik hostPort 80/443, ingress + Authelia on, registry image tags |
| `charts/naslos/values-installer.yaml` | Installer profile: published image repository base, no private registry (FR-INSTALL) |
| `bootstrap/installer/naslos-installer.yaml.tmpl` | Parameterised machine-config patch shipped in the install pack |
| `scripts/deploy-vm.sh` | One-shot VM deployment script |
| `scripts/build-install-pack.sh` | Builds `dist/naslos-install-pack-<version>.tar.gz` |
| `api/Dockerfile`, `agent/Dockerfile`, `ui/Dockerfile` | Container image builds for Naslos components |

### Fixes baked into the deployment (learned the hard way — do not remove)

| Fix | Where | Why |
| --- | --- | --- |
| ZFS installer `factory.talos.dev/installer/4dd8e3a8…f63c:v1.14.1` in `machine.install` | `bootstrap/vm/naslos-vm.yaml` | This is the Image Factory image for `bootstrap/schematic/naslos.yaml` (the official `siderolabs/zfs` extension). Pinned to v1.14.1 to match the validated node. Storage for Kubernetes PVCs is local-path; ZFS pools are agent-managed. |
| `taints: $patch: delete` in `KubeNodeConfig` | `bootstrap/vm/naslos-vm.yaml` | Talos v1.14 default-applies `control-plane:NoSchedule` on a single-node cluster; with the taint, every workload without a toleration stays Pending. |
| No VIP on `eth0` (plain DHCP + reservation) | `bootstrap/vm/naslos-vm.yaml` | VIP == DHCP address fights kubelet node-IP selection (`no suitable node IP found`). |
| `UnattendedInstallConfig` strip after `gen config` | `Makefile` (`bootstrap-vm`) | v1.14 `gen config` emits a stock UIC doc that is mutually exclusive with `machine.install` — apply fails with "UnattendedInstallConfig config is incompatible with v1alpha1 config". |
| `pod-security.kubernetes.io/enforce=privileged` on `local-path-storage`, `naslos` **and** `naslos-privileged` | `scripts/deploy-vm.sh` | The local-path helper pod uses hostPath, and the API mounts hostPath volumes, so baseline PSA rejects them (`violates PodSecurity "baseline:latest": hostPath volumes`). The helper pod runs in the provisioner's namespace and the hostNetwork/privileged workloads live in `naslos-privileged`, so all three need the label. |
| `local-path` patched as default StorageClass | `scripts/deploy-vm.sh` | Chart PVCs don't set `storageClassName`; without a default class they never bind. |
| Helm ownership labels/annotations on the pre-created `naslos` namespace | `scripts/deploy-vm.sh`, `Makefile` (`--take-ownership`) | `helm --create-namespace` fails with "invalid ownership metadata" on an existing namespace; the namespaces and the old kubectl-created OpenLDAP Secrets are adopted into the release with `--take-ownership`. |
| Chart-generated OpenLDAP Secrets (lookup-guarded) | `charts/naslos/templates/openldap-secrets.yaml` | Previously `openldap/generate-secrets.sh` + standalone manifests outside Helm; the chart now generates them on first install and re-emits the existing bytes on upgrade so OpenLDAP is never re-keyed. |
| `TALOS_ENDPOINTS` env on the API pod | `charts/naslos/templates/api-deployment.yaml` (+ `api.talosEndpoints`) | The talos client fatals at startup with "failed to determine endpoints" without it. |
| Agent degraded mode when ZFS is missing | `agent/cmd/main.go` + `agent/internal/server/server.go` | The agent used to `log.Fatalf` without `/dev/zfs`; now it starts and ZFS endpoints return 503 instead of crash-looping. |
| No `olcTLSCipherSuite` in the slapd config LDIF | `openldap/image/entrypoint.sh` | Debian's OpenLDAP is built against GnuTLS, which rejects OpenSSL cipher strings (`HIGH:!aNULL:!MD5`) — `slapadd -n 0` fails on `cn=config` with an opaque error. |
| LDIF continuation lines with a single leading space | `openldap/image/entrypoint.sh` | Four-space continuations corrupt `olcAccess` values. |
| `--skip-crds` on Helm install | `Makefile` (`install`, `install-vm`) | Avoids CRD ownership conflicts with previously kubectl-applied CRDs. |
| Golang builder image ≥ the version required by `cosi-project/runtime` (currently `golang:1.26-alpine`, `go 1.26.5` in go.mod) | `api/Dockerfile`, `agent/Dockerfile` | Older toolchains fail `go mod download` with "requires go >= 1.26.5". |

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

### First install / creating the operator

The chart does not seed a human account, and `/api/users` is an owner route, so
a bare `kubectl port-forward` POST now returns `401`: the request must carry the
`X-Naslos-Proxy-Secret` (Secret `naslos-proxy`) plus `Remote-User`/
`Remote-Groups`, from a source the API's NetworkPolicy allows. The working
options (an in-cluster curl from the Traefik/UI/`naslos-privileged` pods, or a
direct `ldapadd`) are in
[identity-sso.md](identity-sso.md#deployment--first-admin).

```bash
make install-vm
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
git revert <sha> && make install-vm
```

Pools, datasets and the state PVC (shares, buddy identity, notifications) are
untouched by a rollback.

**Operator locked out (lost the TOTP device, Authelia DB corruption, portal
loop).** In order, least destructive first:

1. **Another factor still works?** Log in with a registered WebAuthn device, or
   the recovery/one-time codes if they were saved. The elevated-session one-time
   code is written to the Authelia volume and can be read with
   `kubectl -n naslos exec daemonset/naslos-authelia -- cat /config/notification.txt`.
2. **Session/identity checks.** Confirm the Authelia pod is running and can bind
   LDAP: its logs show `LDAP Result Code 49` when the bind password and the
   `naslos-openldap` Secret's `service-password` disagree. Fix by aligning the
   Secret (never by committing a literal).
3. **Reset the second factor from the DB.** Stop Authelia
   (`kubectl -n naslos scale daemonset/naslos-authelia --replicas=0`), then either
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
| `agent.*` | privileged, host mounts | DaemonSet per node |
| `zfs.arcMax` | 0 (auto) | ARC sizing |
| `shares.*` | SMB/NFS on, AFP off | see [shares.md](shares.md) |
| `traefik.*` | web :80, websecure :443 | LoadBalancer service, dashboard API on with `basePath: /traefik`; `values-vm` adds `ports.*.hostPort` |
| `authelia.*` | domain `naslos.local` | config map; LDAP backend in template; portal at `/authelia` when ingress is on |
| `ingress.*` | disabled | Traefik IngressRoutes + middlewares; `ingress.tls` (secretName/existingSecret) |
| `auth.*` | generated shared secret | owner-route gate (NAS-001); no `disabled` switch exists |
| `ntfy.server.url` | empty ⇒ public `ntfy.sh` | no bundled ntfy chart; see [notifications.md](notifications.md) |
| `prometheus.*` | 30d retention | see [monitoring.md](monitoring.md) |
| `grafana.*` | disabled | removed 2026-09-19 (AUDIT-H4); re-enabling needs a credential from a Secret |
| `storage.*` | localPath only | see [storage-zfs.md](storage-zfs.md) |
| `openldap.*` | bind DN + image | the bind/admin passwords are generated by the chart into the `naslos-openldap` Secret (lookup-guarded, preserved across upgrades); nothing is committed |
| `workloadNamespace` | `naslos-privileged` | namespace for the hostNetwork/privileged workloads (AUDIT-M6) |
| `namespace` | (removed) | every template uses `.Release.Namespace`, so `helm -n <ns>` decides it (AUDIT-L9) |

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
`install-pack` GitHub Actions workflow attaches it, and the checksum file, to a
semantic `vX.Y.Z` release, then dispatches `AessemOps/Naslos-Installer` so the
installer rebuilds against it. The installer resolves the **newest `vX.Y.Z`
tag** (it ignores non-semver tags such as `latest`) and derives its Talos /
schematic gate from the pack's `metadata.json`.

`values-installer.yaml` is the installer profile: it parameterises the image
repository base and **must not** reference the private VM registry. The engine
overrides `domain`, `sso.domains`, `shares.discovery.name`, `openldap.host` and
the `networkPolicy.*` CIDRs at install time.

## Uninstall

```bash
make uninstall
# NOTE: this does NOT delete ZFS pools or PVCs. Clean those up explicitly.
```