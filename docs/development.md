# Development

Guide for working with the Naslos codebase: layout, builds, and how to extend.

## Repository layout

```
Naslos-Linux/
├── rust/                cargo workspace
│   ├── api/             Rust HTTP API (the brain) + buddyctl / buddy-receiver bins
│   └── agent/           Rust privileged DaemonSet for ZFS (chroot /host)
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
| API | `cd rust && cargo build --release -p naslos-api` | `rust/target/release/naslos-api` |
| Agent | `make agent` | `rust/target/release/naslos-agent` |
| UI | `make ui` | `ui/dist` (static) |
| All | `make all` | agent + UI |
| Dev cluster | `make dev-cluster` | local Talos cluster |

UI dev (hot reload): `cd ui && npm run dev` — Vite proxies `/api` to
`http://localhost:8080`.

Container images are built upstream from these binaries (see
[deployment.md](deployment.md#images)). Node/Rust versions: node 20+, Rust 1.88+.

## Testing

Rust tests are colocated as `#[cfg(test)]` modules and in `rust/api/tests/`.
`scripts/audit.sh` is the sweep (`cargo fmt/clippy/test`, `svelte-check`,
`helm lint`, the install-pack checks and `gitleaks` when installed); CI runs it
on every PR. A minimal manual flow:

```bash
sh scripts/audit.sh
cd rust && cargo test
```

For a live smoke test: `make dev-cluster && make crds && make install`, then
`kubectl port-forward -n naslos svc/naslos-ui 8080:80`.

## Extending — add a catalog app

Apps now live in the **NaslosCharts** git repository (see
[app-catalog.md](app-catalog.md) for the repository contract), not in the Rust sources.

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

1. Add the protocol constant in `rust/api/src/shares.rs`.
2. Extend `GenerateSambaConfig` / add `Generate<Proto>Config`.
3. Add the API route in `rust/api/src/server/shares.rs` if config generation is
   needed (+ API doc in [api.md](api.md)).
4. Add a UI form if needed (`ui/src/lib/components/ShareForm.svelte`).
5. Add the chart flag under `values.yaml: shares.<proto>`.

## Emphasis on reviewing AI-generated code

This project is AI-assisted. Before trusting anything in production:

- **Audit the auth boundary** (`rust/api/src/auth.rs`): trust identity headers only
  from the Traefik CIDR **and** only with the `X-Naslos-Proxy-Secret`, never from
  remote IPs.
- **Review privileged surfaces**: `agent` runs as root with host mounts; keep
  the `:9090` port behind cluster networking.
- **Review secrets**: no plaintext secrets in charts; rotate defaults.
- **Test destructive paths** (pool destroy, disk wipe, group delete) in a dev
  cluster before touching real data.

See also [architecture.md](architecture.md#security-model) for the trust
model.