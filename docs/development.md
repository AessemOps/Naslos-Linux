# Development

Guide for working with the Naslos codebase: layout, builds, and how to extend.

## Repository layout

```
Naslos-Linux/
├── api/                 Go — HTTP API server (the brain)
│   └── internal/
│       ├── auth/        header-trust middleware (Remote-*, TRAEFIK_CIDR + proxy secret)
│       ├── catalog/     built-in app store + JSON Schema entries
│       ├── config/      flag/env config type
│       ├── helm/        Helm SDK wrapper (install/upgrade/…)
│       ├── identity/    OpenLDAP client + Samba NT-hash sync
│       ├── metrics/     SystemMetrics snapshot
│       ├── notifications/ ntfy settings + delivery
│       ├── server/      HTTP routes, WebSocket log/exec
│       ├── shares/      share CRUD + smb.conf / NFS-Ganesha config generation
│       └── talos/       Talos machinery client, VolumeAdvisor, volumes
├── agent/               Go privileged DaemonSet for ZFS (chroot /host)
│   └── internal/
│       ├── server/      agent HTTP API (:9090)
│       └── zfs/         pool/dataset/snapshot via chroot /host
├── ui/                  SvelteKit + Tailwind + @xterm
├── charts/naslos/        umbrella Helm chart (api, ui, agent, deps…)
├── bootstrap/schematic/ Image Factory schematic (ZFS extension)
├── bootstrap/cilium/    pinned Cilium manifest spliced into the machine config
├── openldap/            OpenLDAP image + manifests (StatefulSet, jobs)
├── samba/               Samba image + config
├── nfs/                 NFS-Ganesha image + config
├── terminal/            web-terminal image (+ zsh-terminal/)
├── scripts/             deploy-vm.sh, render-cilium.sh, audit.sh
├── Makefile             build/install/dev targets
└── docs/                this documentation set
```

## Build

| Component | Command | Output |
| --- | --- | --- |
| API | `make api` | `bin/naslos-api` |
| Agent | `make agent` | `bin/naslos-agent` |
| UI | `make ui` | `ui/dist` (static) |
| All | `make all` | above three |
| Dev cluster | `make dev-cluster` | local Talos cluster |

UI dev (hot reload): `cd ui && npm run dev` — Vite proxies `/api` to
`http://localhost:8080`.

Container images are built upstream from these binaries (see
[deployment.md](deployment.md#images)). Node/go versions: go 1.26.5+, node 20+.

## Testing

Unit tests are colocated as `_test.go` files. `scripts/audit.sh` is the sweep
(build, vet, `go test -race`, `svelte-check`, `helm lint`, plus `govulncheck`,
`gosec`, `staticcheck` when installed); there is no CI workflow right now, so run
it manually. A minimal manual flow:

```bash
sh scripts/audit.sh
cd api && go build ./... && go vet ./...
cd agent && go build ./... && go vet ./...
```

For a live smoke test: `make dev-cluster && make crds && make install`, then
`kubectl port-forward -n naslos svc/naslos-ui 8080:80`.

## Extending — add a catalog app

Apps now live in the **NaslosCharts** git repository (see
[app-catalog.md](app-catalog.md) for the repository contract), not in Go source.

1. Create `apps/<name>/` in the chart repository with `Chart.yaml`,
   `values.yaml`, `templates/` and a `naslos-app.yaml` install-config.
2. Set `name` equal to the folder name (DNS-1123 label); declare `services[]`
   (the route target, port, scheme) and `exposure` defaults, and a JSON Schema
   under `schema` — it drives `SchemaForm`.
3. Commit to the appropriate channel branch (`Prod` for stable) and refresh:
   `POST /api/sources/refresh` (or the Sources tab), then install from the UI.
4. To test a local change without the git remote, add a source pointing at a
   local file path or a temporary branch and refresh that source only
   (`POST /api/sources/refresh?name=<source>`).

## Adding a share protocol

1. Add the protocol constant in `api/internal/shares` (`share.go`).
2. Extend `GenerateSambaConfig` / add `Generate<Proto>Config`.
3. Add the API route in `api/internal/server/shares.go` if config generation is
   needed (+ API doc in [api.md](api.md)).
4. Add a UI form if needed (`ui/src/lib/components/ShareForm.svelte`).
5. Add the chart flag under `values.yaml: shares.<proto>`.

## Emphasis on reviewing AI-generated code

This project is AI-assisted. Before trusting anything in production:

- **Audit the auth boundary** (`api/internal/auth`): trust identity headers only
  from the Traefik CIDR **and** only with the `X-Naslos-Proxy-Secret`, never from
  remote IPs.
- **Review privileged surfaces**: `agent` runs as root with host mounts; keep
  the `:9090` port behind cluster networking.
- **Review secrets**: no plaintext secrets in charts; rotate defaults.
- **Test destructive paths** (pool destroy, disk wipe, group delete) in a dev
  cluster before touching real data.

See also [architecture.md](architecture.md#security-model) for the trust
model.