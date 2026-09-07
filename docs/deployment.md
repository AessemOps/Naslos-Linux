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
| `ghcr.io/nasos/nasos-api` | `make api` + container build |
| `ghcr.io/nasos/nasos-ui` | `make ui` + container build |
| `ghcr.io/nasos/nasos-agent` | `make agent` + container build |
| `ghcr.io/nasos/nasos-openldap` | `openldap/image/Dockerfile` |

## Make targets

| Target | What it does |
| --- | --- |
| `make all` | Build api, agent, ui |
| `make api` | `go build -o ../bin/nasos-api ./cmd` (api dir) |
| `make agent` | `go build -o ../bin/nasos-agent ./cmd` (agent dir) |
| `make ui` | `npm install && npm run build` (ui dir) |
| `make bootstrap` | Bundle installer ISO/schematic tar |
| `make crds` | Apply Traefik CRDs from `helm show crds traefik/traefik` |
| `make install` | `helm dependency update && helm upgrade --install nasos charts/nasos -n nasos --create-namespace` |
| `make uninstall` | `helm uninstall nasos -n nasos` |
| `make dev-cluster` | `talosctl cluster create --name nasos-dev` from the rendered schematic |

## Install flow

```bash
# 1. Image Factory schematic (ZFS extension)
make bootstrap

# 2. (Local dev) spin a cluster
make dev-cluster

# 3. Apply Traefik CRDs (required before first Helm install)
make crds

# 4. Install NasOS
make install

# 5. Access the UI
kubectl port-forward -n nasos svc/nasos-ui 8080:80
```

## Helm values walkthrough (`charts/nasos/values.yaml`)

| Key | Default | Notes |
| --- | --- | --- |
| `api.*` | enabled, 1 replica, port 8080 | ClusterIP service |
| `ui.*` | enabled, 1 replica, port 80 | ClusterIP service |
| `agent.*` | privileged, host mounts | DaemonSet per node |
| `zfs.arcMax` | 0 (auto) | ARC sizing |
| `shares.*` | SMB/NFS on, AFP off | see [shares.md](shares.md) |
| `traefik.*` | web :80, websecure :443 | LoadBalancer service, dashboard off |
| `authelia.*` | domain `nasos.local` | config map; LDAP backend in template |
| `ntfy.server.url` | empty ⇒ bundled ntfy | see [notifications.md](notifications.md) |
| `prometheus.*` | 30d retention | see [monitoring.md](monitoring.md) |
| `grafana.*` | adminPassword `nasos-admin` | **change** |
| `storage.*` | localPath + ZFS LocalPV | see [storage-zfs.md](storage-zfs.md) |
| `openldap.*` | bind DN/password | **change** the default secret |
| `namespace` | `nasos` | everything deploys here |

## Secrets to rotate before production

- `openldap.bindPassword` / `admin-password` + `service-password`
  (`kubectl create secret generic nasos-openldap …`)
- `grafana.adminPassword`
- `authelia-config` `jwt_secret` (generated at install — rotate if it shows up
  in git/diffs)
- Internal CA (`nasos-openldap-tls` secret) — replace with a real CA if you
  want browser-trusted HTTPS.

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