# AGENTS.md

Instructions for AI agents (and humans in a hurry) working in the Naslos repo.
This file is the stable operating manual: commands, workflow, and conventions.
For **current state** — what is deployed, what is open, live drill results — read
[`AI_Handoff.md`](AI_Handoff.md). Normative requirements are in
[`docs/spec.md`](docs/spec.md).

## What this is

A single-node NAS appliance on Talos Linux (Kubernetes) with a web UI: ZFS
pools/datasets, SMB + NFS shares with LDAP identities, a dashboard, web
terminal, app catalog, dynamic DNS, ACME certificates, and zero-knowledge peer
backups ("Buddy").

## Layout

| Path | What it is |
| --- | --- |
| `api/` | Go HTTP API + scheduler/job runner (Go module `github.com/AessemOps/Naslos-Linux/api`) |
| `agent/` | Privileged, host-networked DaemonSet: the only thing that runs `zpool`/`zfs`/`wipefs` (Go module) |
| `ui/` | Svelte 5 (legacy syntax) + TypeScript + Tailwind, built with adapter-static, served by unprivileged nginx |
| `charts/naslos/` | The Helm chart: api, ui, agent, samba, nfs, terminal, openldap, Traefik + Authelia (+ optional cert-manager) |
| `openldap/ samba/ nfs/ terminal/` | Per-service images and config templates |
| `docs/` | Per-topic reference (`spec.md`, `api.md`, `dynamic-dns.md`, `deployment.md`, `operations.md`, …) |
| `.kilo/plans/` | Implementation plans (committed). Put new plans here. |
| `.kilo/command/`, `.kilo/agent/` | Kilo project commands and agents (do not use `.kilocode/` or `.opencode/`) |

## Commands

There are **no `make test` / `make lint` targets** — run the gates directly.

```bash
# Go (both modules)
cd api   && go build ./... && go vet ./... && go test -race ./...
cd agent && go build ./... && go vet ./... && go test -race ./...

# UI type-check + build
cd ui && npm run check && npm run build

# Chart
helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml
helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-installer.yaml

# Install pack for the desktop installer (FR-INSTALL; docs/installer-contract.md)
make install-pack

# Full sweep (go + UI + other checks)
sh scripts/audit.sh
```

Playwright e2e runs against the **live VM** at `https://naslos.local` and needs
the gitignored `ui/.env.playwright.local` (admin password + TOTP secret). Run the
local binary, not a bare `npx playwright` (npx can resolve a different global
Playwright and fail with "did not expect test.describe() to be called here"):

```bash
cd ui && ./node_modules/.bin/playwright test                 # whole suite
cd ui && ./node_modules/.bin/playwright test tests/domains.spec.ts
```

The suite logs into Authelia once (`tests/auth.setup.ts`) and reuses the session
via `storageState`; a target that does not challenge is an error.

## Workflow

- **Always commit and push your work** — never leave uncommitted changes at the
  end of a session.
- **Never push to `master`** without explicit user approval. Work on a branch
  (e.g. `feature/…`, `fix/…`, `chore/…`) and open a PR against `master`. Fetch
  first; `origin/master` may be ahead of a stale local `master`.
- Commit messages follow Conventional Commits (`feat(scope): …`,
  `fix(scope): …`, `docs(scope): …`, `chore(scope): …`).
- Write a plan in `.kilo/plans/` for any non-trivial change and commit it.
- Only commit/push/create PRs when the work is complete and verified. Never
  force-push, skip hooks, or amend a failed commit — add a new commit.
- **Cross-repo installer order**: the desktop installer lives in
  `AessemOps/Naslos-Installer` and embeds the newest semantic `vX.Y.Z` install
  pack. Land the Naslos-Linux pack + `docs/installer-contract.md` changes first
  (release a `vX.Y.Z` pack), then the installer work that consumes them; the
  `install-pack` workflow dispatches the installer to rebuild against the new
  pack. A change to any interface in the contract doc must update the installer
  in the same release.

## Conventions

- **Spec rule**: a change that alters a MUST in `docs/spec.md` updates the spec
  *and* its test in the same change; unimplemented MUSTs are marked `[OPEN]`.
- **Credits rule**: `CREDITS.md` is always kept current. A change that
  ports/adapts code or config from another project adds a provenance header to
  the ported file; a change to a direct dependency (`api/go.mod`,
  `ui/package.json`, `charts/naslos/Chart.yaml`, or a Dockerfile base image)
  updates `CREDITS.md` **in the same change** and bumps its `Last reviewed`
  line.
- **Found a defect while drilling?** Fix it in the same branch with a test, and
  record it where the next session will read it (handoff/plan).
- **Every fix must be in a fresh install.** A fix belongs in the declarative
  install path — the `Makefile` (targets/prerequisites), `charts/naslos/`
  (`values.yaml`, the VM overlay `values-vm.yaml` and the installer overlay
  `values-installer.yaml`), or
  `scripts/deploy-vm.sh` — and in the images built at install time, never only
  in the live cluster. A change applied with a one-off `kubectl`, a live `helm
  --set`, or a manual `helm upgrade` is **incomplete**: wire it into the
  chart/Makefile/values and bump the image tag so `make install-vm` on a clean
  node reproduces it. When touching install plumbing, prove it with a dry run
  (`make -n install-vm`) and `helm template`/`helm lint`; a fix that needs
  operator-only config (credentials, external API rights) must be documented as
  such so the install gap is explicit.
- **Tests before hand-off**: `go build/vet/test` for both Go modules,
  `npm run check`, `helm lint`, the Playwright suite for UI/API changes, and a
  live drill for anything touching the node (shares, LDAP, ZFS, backups).
- **Docs rule**: `AI_Handoff.md` and the per-topic `docs/` (`spec.md`, `api.md`,
  `deployment.md`, `operations.md`, `installer-contract.md`, …) are always kept
  up to date. A change that
  alters behavior, an endpoint, a config key or env var, the chart/install path,
  or the deployed state updates every affected doc **in the same change** — a doc
  that no longer matches the code makes the change incomplete. `AI_Handoff.md`
  stays the ~1-page "deployed right now" page (image tags, helm revision, open
  PRs, live-drill results); new session narratives belong in `docs/archive/`, not
  there. When a session ends, the handoff reflects what is actually deployed —
  never stale tags or revisions.
- **README rule**: [`README.md`](README.md) is always kept up to date. A change
  that alters what the project is, the feature list, the repo layout, the setup
  commands, or the install/deploy path updates the README **in the same change**.
- Third-party attribution lives in [`CREDITS.md`](CREDITS.md).

## Deploying to the VM

- **Always bump image tag suffixes.** The registry reuses tags and the chart
  pulls with `IfNotPresent`, so a retag alone can keep running old code. Build
  and push with a fresh `IMAGE_TAG` (e.g. `0.1.0-r30`), then set it in
  `charts/naslos/values-vm.yaml` (or `--set`).
- `make install-vm` renders `-f values.yaml -f values-vm.yaml` (it is **not**
  `--reuse-values`) and now also installs its prerequisites first: `crds`,
  `cert-manager`, and `cert-manager-webhook-ovh`.
- OVH certificates need cert-manager **and** the `cert-manager-webhook-ovh`
  webhook (OVH is not a cert-manager built-in solver). They install as separate
  releases in the `cert-manager` namespace — deliberately outside the `naslos`
  default-deny network policies. The provider's `webhook.groupName`
  (`ovh.naslos.local`) must match `OVH_WEBHOOK_GROUP` in the Makefile. The OVH
  API token must grant `GET/POST/PUT/DELETE /domain/zone/<zone>/*` (the
  `/status`, `/record` and `/refresh` subpaths).
- Verify after deploy: `kubectl -n naslos rollout status deploy/naslos-api`,
  `curl -sk https://naslos.local/api/health`, and the relevant Playwright spec.
- Record the new tags and revision in `AI_Handoff.md` → "Deployed right now".

## Environment facts

- Node `192.168.1.117`, UI `https://naslos.local` (the only listener; no
  NodePort), private registry `192.168.1.2:30095`, `TALOSCONFIG=bootstrap/vm/talosconfig`.
- Namespaces: `naslos` (authenticated services), `naslos-privileged`
  (agent/samba/nfs/terminal), `naslos-apps` (installed apps, PSA baseline),
  `naslos-apps-priv` (privileged apps), `cert-manager`.
- Auth is Authelia forwardAuth only (`/authelia`, 2FA for `naslos_admins`); the
  UI signs out by `POST /authelia/api/logout`. There is no unauthenticated
  posture or bypass.
- The API is owner-gated and restricted to the proxy/pod CIDR; query it from a
  trusted pod (e.g. `kubectl -n naslos-privileged exec deploy/naslos-terminal`)
  with `Remote-User`, `Remote-Groups`, and `X-Naslos-Proxy-Secret`, not from the
  workstation.
- Read [`AI_Handoff.md`](AI_Handoff.md) → "Operational gotchas" before drilling;
  it lists the hard-won traps (mount propagation, PSA, egress, `--reuse-values`,
  image tags).
