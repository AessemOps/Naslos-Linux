# Development

Guide for working with the NasOS codebase: layout, builds, and how to extend.

## Repository layout

```
Nasos-Linux/
├── api/                 Go — HTTP API server (the brain)
│   └── internal/
│       ├── auth/        header-trust middleware (Remote-*, TRAEFIK_CIDR)
│       ├── catalog/     built-in app store + JSON Schema entries
│       ├── config/      flag/env config type
│       ├── helm/        Helm SDK wrapper (install/upgrade/…)
│       ├── identity/    OpenLDAP client + Samba NT-hash sync
│       ├── metrics/     SystemMetrics snapshot
│       ├── notifications/ ntfy settings + delivery
│       ├── server/      HTTP routes, WebSocket log/exec
│       ├── shares/      share CRUD + smb.conf / /etc/exports generation
│       └── talos/       Talos machinery client, VolumeAdvisor, volumes
├── agent/               Go privileged DaemonSet for ZFS (chroot /host)
│   └── internal/
│       ├── server/      agent HTTP API (:9090)
│       └── zfs/         pool/dataset/snapshot via chroot /host
├── ui/                  SvelteKit + Tailwind + xterm
├── charts/nasos/        umbrella Helm chart (api, ui, agent, deps…)
├── bootstrap/schematic/ Image Factory schematic (ZFS extension)
├── openldap/            OpenLDAP image + manifests (StatefulSet, jobs)
├── zsh-terminal/        web-terminal container image
├── Makefile             build/install/dev targets
└── docs/                this documentation set
```

## Build

| Component | Command | Output |
| --- | --- | --- |
| API | `make api` | `bin/nasos-api` |
| Agent | `make agent` | `bin/nasos-agent` |
| UI | `make ui` | `ui/dist` (static) |
| All | `make all` | above three |
| Dev cluster | `make dev-cluster` | local Talos cluster |

UI dev (hot reload): `cd ui && npm run dev` — Vite proxies `/api` to
`http://localhost:8080`.

Container images are built upstream from these binaries (see
[deployment.md](deployment.md#images)). Node/go versions: go 1.22+, node 20+.

## Testing

Unit tests are colocated as `_test.go` files (currently no suite is wired into
the Makefile). CI-style flow:

```bash
cd api && go build ./... && go vet ./...
cd agent && go build ./... && go vet ./...
```

For a live smoke test: `make dev-cluster && make crds && make install`, then
`kubectl port-forward -n nasos svc/nasos-ui 8080:80`.

## Extending — add a catalog app

1. Pick a Nest chart + repo (see existing `builtin*` entries for the pattern).
2. Add an entry to `api/internal/catalog/builtin_extra.go` (or a JSON file in
   the configured `catalogPath` — it overrides same-named built-ins).
3. Fill `schema` (JSON Schema — drives `SchemaForm`) and `defaultValues`.
4. Build: `make api`; test install in dev cluster:
   ```bash
   helm repo add <repo> <url> && helm install <name> <repo>/<chart> -n nasos
   POST /api/apps { "name": "<name>", "values": {} }
   ```
5. Wire anything extra (ingress, storage) via chart `values`.

## Adding a share protocol

1. Add the protocol constant in `api/internal/shares` (`share.go`).
2. Extend `GenerateSambaConfig` / add `Generate<Proto>Config`.
3. Add the API route in `api/internal/server/shares.go` if config generation is
   needed (+ API doc in [api.md](api.md)).
4. Add a UI form if needed (`ui/src/lib/components/ShareForm.svelte`).
5. Add the chart flag under `values.yaml: shares.<proto>`.

## Emphasis on reviewing AI-generated code

This project is AI-assisted. Before trusting anything in production:

- **Audit the auth boundary** (`api/internal/auth`): trust only the Traefik
  CIDR, never remote IPs.
- **Review privileged surfaces**: `agent` runs as root with host mounts; keep
  the `:9090` port behind cluster networking.
- **Review secrets**: no plaintext secrets in charts; rotate defaults.
- **Test destructive paths** (pool destroy, disk wipe, group delete) in a dev
  cluster before touching real data.

See also [architecture.md](architecture.md#security-model) for the trust
model.