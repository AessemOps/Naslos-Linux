# Deployment

## Prerequisites

| Tool | Version |
| --- | --- |
| [talosctl](https://www.talos.dev/latest/talosctl/installation/) | 1.14+ |
| [kubectl](https://kubernetes.io/docs/tasks/tools/) | current |
| [helm](https://helm.sh/docs/intro/install/) | current |
| docker / podman | current |
| go | 1.22+ (build api/agent) |
| node | 20+ (build ui) |

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
| `make crds` | Apply Traefik CRDs from `helm show crds traefik/traefik` |
| `make install` | `helm dependency update && helm upgrade --install naslos charts/naslos -n naslos --create-namespace` |
| `make install-vm` | `make install` with the VM-specific values override (dev posture: NodePort UI, auth off) |
| `make install-prod` | `install-vm`'s values plus `values-prod.yaml`: Traefik on hostPort 80/443 with Authelia forwardAuth (see below) |
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

After bootstrapping, fetch Kubernetes credentials and deploy OpenLDAP:

```bash
# Make kubectl/helm talk to the new cluster
talosctl kubeconfig --nodes 192.168.1.96 --endpoints 192.168.1.96 -f ~/.kube/config

# Deploy local-path-provisioner (single-node VM, no ZFS pool required)
kubectl apply -f https://raw.githubusercontent.com/rancher/local-path-provisioner/v0.0.26/deploy/local-path-storage.yaml
kubectl wait --for=condition=ready pod -l app=local-path-provisioner -n local-path-storage --timeout=120s

# Create the OpenLDAP password + TLS secrets
./openldap/generate-secrets.sh

# Deploy OpenLDAP
kubectl apply -f openldap/manifests/statefulset.yaml
kubectl apply -f openldap/manifests/bootstrap-job.yaml
kubectl apply -f openldap/manifests/backup-cronjob.yaml
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

The VM override exposes the UI on a NodePort:

```
http://192.168.1.96:30080
```

### Automated deployment script

For convenience, `scripts/deploy-vm.sh` runs the full flow (build images,
generate config, apply, bootstrap, install). It defaults to the private HTTP
registry `192.168.1.2:30095` with the password `secret`. Review it before running:

```bash
chmod +x scripts/deploy-vm.sh
./scripts/deploy-vm.sh
```

Override the registry and/or secret as needed:

```bash
REGISTRY=docker.io/myuser REGISTRY_HTTP_SECRET=mypassword IMAGE_TAG=latest \
  ./scripts/deploy-vm.sh
```

> Note: The default passwords are unchanged (`grafana.adminPassword: naslos-admin`,
> `openldap.bindPassword: CHANGE_ME_SERVICE_PASSWORD`). Rotate them before any
> real use.

### Files added for the VM

| Path | Purpose |
| --- | --- |
| `bootstrap/vm/naslos-vm.yaml` | Machine-config patch for the VM installer/network/cluster |
| `bootstrap/vm/controlplane.yaml` | Generated by `make bootstrap-vm` |
| `bootstrap/vm/talosconfig` | Generated by `make bootstrap-vm` |
| `charts/naslos/values.yaml` | Helm values: IP-based domain, NodePort UI, ClusterIP Traefik, ZFS LocalPV, bundled ntfy, openldap secret template |
| `charts/naslos/values-vm.yaml` | Helm override (dev posture): IP-based domain, NodePort UI, ClusterIP Traefik |
| `charts/naslos/values-prod.yaml` | Helm override (production posture): ingress on, auth on, NodePort off, `naslos.local`, Traefik hostPort 80/443 |
| `scripts/deploy-vm.sh` | One-shot VM deployment script |
| `api/Dockerfile`, `agent/Dockerfile`, `ui/Dockerfile` | Container image builds for Naslos components |

### Fixes baked into the deployment (learned the hard way — do not remove)

| Fix | Where | Why |
| --- | --- | --- |
| Stock (ZFS-less) installer `factory.talos.dev/installer/3765…3b4ba:v1.14.0` in `machine.install` | `bootstrap/vm/naslos-vm.yaml` | The `siderolabs/zfs` extension never loads `/dev/zfs` in this VM; the node then hangs in `startAllServices` waiting for `ext-zfs-service` and kubelet never starts. Storage is covered by local-path. |
| `taints: $patch: delete` in `KubeNodeConfig` | `bootstrap/vm/naslos-vm.yaml` | Talos v1.14 default-applies `control-plane:NoSchedule` on a single-node cluster; with the taint, every workload without a toleration stays Pending. |
| No VIP on `eth0` (plain DHCP + reservation) | `bootstrap/vm/naslos-vm.yaml` | VIP == DHCP address fights kubelet node-IP selection (`no suitable node IP found`). |
| `UnattendedInstallConfig` strip after `gen config` | `Makefile` (`bootstrap-vm`) | v1.14 `gen config` emits a stock UIC doc that is mutually exclusive with `machine.install` — apply fails with "UnattendedInstallConfig config is incompatible with v1alpha1 config". |
| `pod-security.kubernetes.io/enforce=privileged` on `local-path-storage` **and** `naslos` namespaces | `scripts/deploy-vm.sh` | The local-path helper pod uses hostPath; baseline PSA rejects it and PVC provisioning fails (`violates PodSecurity "baseline:latest": hostPath volumes`). The helper pod is created in the provisioner's namespace, so **both** need the label. |
| `local-path` patched as default StorageClass | `scripts/deploy-vm.sh` | Chart PVCs don't set `storageClassName`; without a default class they never bind. |
| Helm ownership labels/annotations on the pre-created `naslos` namespace | `scripts/deploy-vm.sh` | `helm --create-namespace` fails with "invalid ownership metadata" on an existing namespace. |
| `TALOS_ENDPOINTS` env on the API pod | `charts/naslos/templates/api-deployment.yaml` (+ `api.talosEndpoints`) | The talos client fatals at startup with "failed to determine endpoints" without it. |
| Agent degraded mode when ZFS is missing | `agent/cmd/main.go` + `agent/internal/server/server.go` | The agent used to `log.Fatalf` without `/dev/zfs`; now it starts and ZFS endpoints return 503 instead of crash-looping. |
| No `olcTLSCipherSuite` in the slapd config LDIF | `openldap/image/entrypoint.sh` | Debian's OpenLDAP is built against GnuTLS, which rejects OpenSSL cipher strings (`HIGH:!aNULL:!MD5`) — `slapadd -n 0` fails on `cn=config` with an opaque error. |
| LDIF continuation lines with a single leading space | `openldap/image/entrypoint.sh` | Four-space continuations corrupt `olcAccess` values. |
| `--skip-crds` on Helm install | `Makefile` (`install`, `install-vm`) | Avoids CRD ownership conflicts with previously kubectl-applied CRDs. |
| Golang builder image ≥ the version required by `cosi-project/runtime` (currently `golang:1.26-alpine`, `go 1.26.5` in go.mod) | `api/Dockerfile`, `agent/Dockerfile` | Older toolchains fail `go mod download` with "requires go >= 1.26.5". |

## Production posture: Authelia + Traefik (`values-prod.yaml`)

`values-vm.yaml` is the **dev** posture: the UI is reached over its NodePort
(`:30080`) and `auth.disabled=true`, so the API serves owner routes to any
client. `values-prod.yaml` layers on top of it and puts the instance behind the
proxy:

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
- **TLS**: the chart generates a self-signed certificate for `authelia.domain`
  on first install and reuses it across upgrades
  (`templates/tls-secret.yaml`, `lookup`-guarded). Import the `naslos-tls`
  Secret's `tls.crt` on a client to remove the browser warning, or point
  `ingress.tls.existingSecret` at a Secret from a real CA (keys `tls.crt` /
  `tls.key`).
- **2FA**: Authelia requires `two_factor` on every admin-only prefix (users,
  groups, apps, volumes, datasets, disks, shares, pods, namespaces, ws, and the
  owner buddy routes). Enroll TOTP/WebAuthn at the first portal login.
- **The NodePort is disabled**: it cannot carry the proxy secret the API needs,
  and it would expose every route unauthenticated if the API's check were ever
  off.
- **Traefik dashboard**: `https://naslos.local/traefik/dashboard/`, linked from
  the sidebar for administrators only. The API is not exposed on any port
  (`api.insecure` off) and Authelia denies non-admin accounts on `/traefik`.

### Cutover

The operator account must exist **before** the flip — with `auth.disabled=true`
the NodePort can create it; once auth is on, only the proxy reaches the API:

```bash
# 1. (while still on the dev posture) create the admin
curl -X POST http://192.168.1.117:30080/api/users \
  -H 'Content-Type: application/json' \
  -d '{"uid":"admin","firstName":"Admin","lastName":"User",
       "password":"<strong password>","groups":["naslos_admins"]}'

# 2. lint both profiles, then cut over
make install-prod
```

Verify, in order: `kubectl -n naslos get ingressroute` shows `naslos-ui`,
`naslos-api`, `naslos-authelia-portal` and `naslos-redirect`;
`curl -sk https://naslos.local/api/health` is 200;
`curl -skI https://naslos.local/api/users` redirects to `/authelia` (not 200);
the browser reaches `/authelia`, logs in, registers 2FA, and lands on the
dashboard; the Terminal page works (it proves `/api` goes straight to the API);
`http://192.168.1.117:30080` no longer serves the app.

### Rollback

Re-apply the dev profile (do **not** pass `--reuse-values`: it would keep the
production keys):

```bash
helm upgrade --install naslos charts/naslos -n naslos \
  -f charts/naslos/values.yaml \
  -f charts/naslos/values-vm.yaml
```

Pools, datasets and the state PVC (shares, buddy identity, notifications) are
untouched by either posture.

> The Playwright suite authenticates by sending the identity headers directly, so
> it can only run against the **dev** posture. Run it before the cutover, or
> roll back to `values-vm.yaml` to run it.

## Helm values walkthrough (`charts/naslos/values.yaml`)

| Key | Default | Notes |
| --- | --- | --- |
| `api.*` | enabled, 1 replica, port 8080 | ClusterIP service |
| `ui.*` | enabled, 1 replica, port 80 | ClusterIP service |
| `agent.*` | privileged, host mounts | DaemonSet per node |
| `zfs.arcMax` | 0 (auto) | ARC sizing |
| `shares.*` | SMB/NFS on, AFP off | see [shares.md](shares.md) |
| `traefik.*` | web :80, websecure :443 | LoadBalancer service, dashboard off; `values-prod` adds `ports.*.hostPort` |
| `authelia.*` | domain `naslos.local` | config map; LDAP backend in template; portal at `/authelia` when ingress is on |
| `ingress.*` | disabled | Traefik IngressRoutes + middlewares; `ingress.tls` (secretName/existingSecret) |
| `auth.*` | `disabled=false`, generated shared secret | owner-route gate; unit of truth for `AUTH_DISABLED` (NAS-001) |
| `ntfy.server.url` | empty ⇒ bundled ntfy | see [notifications.md](notifications.md) |
| `prometheus.*` | 30d retention | see [monitoring.md](monitoring.md) |
| `grafana.*` | adminPassword `naslos-admin` | **change** |
| `storage.*` | localPath + ZFS LocalPV | see [storage-zfs.md](storage-zfs.md) |
| `openldap.*` | bind DN/password | **change** the default secret |
| `namespace` | `naslos` | everything deploys here |

## Secrets to rotate before production

- `openldap.bindPassword` / `admin-password` + `service-password`
  (`kubectl create secret generic naslos-openldap …`)
- `grafana.adminPassword`
- `authelia-config` `jwt_secret` (generated at install — rotate if it shows up
  in git/diffs)
- Internal CA (`naslos-openldap-tls` secret) — replace with a real CA if you
  want browser-trusted HTTPS.
- `naslos-tls` (the ingress certificate) is chart-generated and self-signed for
  `authelia.domain`; trust it per client, or set `ingress.tls.existingSecret`
  to a real certificate. To force renewal, delete the Secret and re-run the
  upgrade.

## Upgrades

- **Helm**: `make install` re-runs `helm upgrade --install`, applying new chart
  values and templates.
- **Talos**: `talosctl upgrade`; the ZFS extension comes from the schematic
  (see [bootstrap.md](bootstrap.md#upgrades)).
- **Images**: bump `*.image.tag` in `values.yaml` and re-run `make install`.

## Uninstall

```bash
make uninstall
# NOTE: this does NOT delete ZFS pools or PVCs. Clean those up explicitly.
```