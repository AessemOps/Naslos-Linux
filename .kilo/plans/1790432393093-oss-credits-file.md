# Plan — Open-source credits / attribution file

## Goal

Add a single, human-readable attribution file at the repo root listing every
open-source component Naslos depends on or redistributes, so the project meets
the attribution obligations of its **AGPL-3.0** license and its dependency
licenses. Also fix the README license contradiction and link the new file.

## Locked decisions

- **File:** `CREDITS.md` at the repo root (curated, grouped by layer; not an
  exhaustive generated notice).
- **Maintenance:** static file with a `Last reviewed` line and a short note that
  it must be updated when `api/go.mod`, `ui/package.json`, or
  `charts/naslos/Chart.yaml` change. No generator or Make target.
- **README:** change the License section from "Apache 2.0" to
  "GNU AGPL-3.0" (matches `LICENSE`) and add a link to `CREDITS.md`.
- **Branch:** new branch **from `origin/master`**, created in a **separate git
  worktree**, so the uncommitted `api/internal/ddns/ddns.go` change on
  `feature/ddns-dynamic-dns` is left untouched.

## Branch / worktree setup (implementation agent)

`origin/master` is the up-to-date base (`b2e816c`, 2026-09-26); the local
`master` ref is stale (`8ac3036`). Current worktree has a modified
`api/internal/ddns/ddns.go` that must not move.

```sh
git fetch origin
git worktree add ../Naslos-credits -b chore/oss-credits origin/master
```

Then do all edits in `../Naslos-credits`. Commit and push the branch, and open a
PR (never push to `master`). Suggested commit message:
`docs(credits): add third-party open-source attributions`.

## CREDITS.md content specification

Header: one sentence saying the file credits third-party open-source software
used by or redistributed with Naslos, whose own license is AGPL-3.0 (see
`LICENSE`). State that components not listed here (transitive Go/npm build-time
dependencies) are recorded in `api/go.sum`, `ui/package-lock.json`, and
`charts/naslos/Chart.lock`, and that each remains under its own upstream
license. Add `Last reviewed: <YYYY-MM-DD>` and a short maintenance note.

Use one Markdown table per group. Columns: **Component | Version / Pin |
License (SPDX) | Upstream**.

### 1. Platform & runtime (not vendored, but required runtime deps)

| Component | Version / Pin | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| Talos Linux | v1.14+ (in `bootstrap/`) | MPL-2.0 | https://github.com/siderolabs/talos |
| Kubernetes | v1.33.x | Apache-2.0 | https://github.com/kubernetes/kubernetes |
| Cilium | `bootstrap/cilium/cilium.yaml` | Apache-2.0 | https://github.com/cilium/cilium |
| OpenZFS (`siderolabs/zfs` extension) | `bootstrap/schematic/naslos.yaml` | CDDL-1.0 | https://github.com/openzfs/zfs |
| containerd | Talos-bundled | Apache-2.0 | https://github.com/containerd/containerd |
| etcd | Talos-bundled | Apache-2.0 | https://github.com/etcd-io/etcd |
| CoreDNS | Talos-bundled | Apache-2.0 | https://github.com/coredns/coredns |

### 2. Base / build container images

| Component | Pin | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| Debian (slim base) | `trixie-slim` | Debian-based (mixed, DFSG) | https://www.debian.org/ |
| Alpine Linux (base) | `alpine:3.20` | Alpine-based (musl MIT, BusyBox GPL-2.0-only) | https://alpinelinux.org/ |
| distroless static | `gcr.io/distroless/static-debian12:nonroot` | Apache-2.0 | https://github.com/GoogleContainerTools/distroless |
| nginx unprivileged | `1.30.5-alpine` | BSD-2-Clause | https://github.com/nginxinc/docker-nginx-unprivileged |
| Go toolchain | `golang:1.26-alpine` | BSD-3-Clause | https://go.dev/ |
| Node.js toolchain | `node:20-alpine` | MIT | https://nodejs.org/ |

Sources: all Dockerfiles under `api/`, `ui/`, `agent/`, `samba/`, `nfs/`,
`openldap/`, `terminal/`, `zsh-terminal/`.

### 3. Go modules — direct (from `api/go.mod`)

`agent/go.mod` declares no external modules. List the direct `require` block:

| Component | Version | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| github.com/cosi-project/runtime | v1.16.3 | MPL-2.0 | https://github.com/cosi-project/runtime |
| github.com/go-git/go-git/v5 | v5.16.3 | Apache-2.0 | https://github.com/go-git/go-git |
| github.com/go-ldap/ldap/v3 | v3.4.8 | MIT | https://github.com/go-ldap/ldap |
| github.com/gorilla/websocket | v1.5.4-0.20250319132907-e064f32e3674 | BSD-2-Clause | https://github.com/gorilla/websocket |
| github.com/siderolabs/talos/pkg/machinery | v1.14.0 | MPL-2.0 | https://github.com/siderolabs/talos |
| golang.org/x/crypto | v0.55.0 | BSD-3-Clause | https://cs.opensource.google/go/x/crypto |
| gopkg.in/yaml.v3 | v3.0.1 | MIT + Apache-2.0 | https://github.com/go-yaml/yaml |
| helm.sh/helm/v3 | v3.18.5 | Apache-2.0 | https://github.com/helm/helm |
| k8s.io/api | v0.33.3 | Apache-2.0 | https://github.com/kubernetes/api |
| k8s.io/apimachinery | v0.33.3 | Apache-2.0 | https://github.com/kubernetes/apimachinery |
| k8s.io/cli-runtime | v0.33.3 | Apache-2.0 | https://github.com/kubernetes/cli-runtime |
| k8s.io/client-go | v0.33.3 | Apache-2.0 | https://github.com/kubernetes/client-go |

Add one line: "Indirect Go dependencies (~165 modules) are recorded in
`api/go.sum`; licenses are as published by each module."

### 4. npm packages — direct (from `ui/package.json`)

Runtime dependencies:

| Component | Version | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| @xterm/xterm | ^6.0.0 | MIT | https://github.com/xtermjs/xterm.js |
| @xterm/addon-fit | ^0.11.0 | MIT | https://github.com/xtermjs/xterm.js |

Dev / build dependencies (redistributed only into built static assets, listed
for completeness): `@playwright/test` (Apache-2.0), `@sveltejs/kit`,
`@sveltejs/adapter-auto`, `@sveltejs/adapter-static`,
`@sveltejs/vite-plugin-svelte`, `svelte`, `svelte-check`,
`@types/node` (MIT), `autoprefixer`, `postcss`, `tailwindcss`, `vite` (MIT),
`typescript` (Apache-2.0). Add: "Full transitive tree pinned in
`ui/package-lock.json`."

### 5. Helm chart subcharts (from `charts/naslos/Chart.yaml` / `Chart.lock`)

| Component | Version | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| Traefik | 41.6.0 | MIT | https://github.com/traefik/traefik |
| Authelia | 0.11.22 | Apache-2.0 | https://github.com/authelia/authelia |
| Prometheus (+ Alertmanager) | 25.0.0 | Apache-2.0 | https://github.com/prometheus/prometheus |
| cert-manager | v1.18.2 | Apache-2.0 | https://github.com/cert-manager/cert-manager |

Note that `ntfy` is not a Helm dependency (alerts go to an ntfy server), but the
Naslos API integrates with the ntfy server project — include ntfy under
"integrated services":

| ntfy | integrated (no pinned version) | Apache-2.0 | https://github.com/binwiederhier/ntfy |

### 6. Bundled OS packages

Group by image; each package under its own upstream license. Include at minimum:

- **samba** (`samba/image/Dockerfile`): Samba (GPL-3.0-or-later), smbclient
  (GPL-3.0-or-later), samba-vfs-modules (GPL-3.0-or-later), libnss-extrausers,
  Avahi (LGPL-2.1-or-later), D-Bus (AFL-2.1 OR GPL-2.0-or-later), wsdd2, tini
  (MIT).
- **nfs** (`nfs/image/Dockerfile`): NFS-Ganesha (LGPL-3.0), nfs-ganesha-vfs
  (LGPL-3.0), nfs-common, procps, tini (MIT).
- **openldap** (`openldap/image/Dockerfile`): OpenLDAP slapd (OpenLDAP Public
  License), ldap-utils (OpenLDAP Public License), OpenSSL (Apache-2.0),
  ca-certificates.
- **terminal** (`terminal/image/Dockerfile`): bash (GPL-3.0-or-later), e2fsprogs,
  util-linux, procps, iproute2 (GPL-2.0), iputils, dnsutils (BIND, MPL-2.0),
  curl (curl), ca-certificates, less, nano (GPL-3.0), jq (MIT), tini (MIT).
- **zsh-terminal** (`zsh-terminal/Dockerfile`): zsh (MIT-style), bash (GPL-3.0),
  zsh-autosuggestions (MIT), zsh-syntax-highlighting (BSD-3-Clause), curl,
  wget (GPL-3.0), jq (MIT), vim (Vim), nano (GPL-3.0), htop (GPL-2.0), iotop
  (GPL-2.0), ncdu (MIT), tmux (ISC), openssh-client (BSD), zfs/zfs-utils
  (CDDL-1.0), kubectl (Apache-2.0), talosctl (MPL-2.0).

Add a sentence that Debian/Alpine package licenses are governed by each
upstream package (see the image's `/usr/share/doc/*/copyright`).

### 7. Attribution caveats

Add a short closing note: if a component is redistributed in a built image, its
license and copyright notice remain with it; this file is an index, not a
replacement for the upstream license texts (see each project's own `LICENSE`).

## README edit

In `README.md`, replace the `## License` section (currently lines 93–95):

```md
## License

GNU Affero General Public License v3.0 — see [LICENSE](LICENSE).
Third-party open-source components and their licenses are listed in
[CREDITS.md](CREDITS.md).
```

## Validation

- `git diff --stat` in the new worktree shows only `CREDITS.md` (new) and
  `README.md` (modified) — the ddns file must not appear.
- Every direct dependency in `api/go.mod` (both `require` blocks' direct
  entries), every entry in `ui/package.json`, and every subchart in
  `charts/naslos/Chart.lock` appears in `CREDITS.md` (spot-check with grep for
  the module/package name).
- Markdown renders: tables well-formed, no broken relative links
  (`LICENSE`, `CREDITS.md`).
- License values marked uncertain below were verified against the upstream
  project's own `LICENSE`/SPDX identifier before committing.

## License values needing verification before commit

Confirm these against upstream (do not ship guessed values): wsdd2,
libnss-extrausers, nfs-common, procps, e2fsprogs, util-linux, iputils,
ca-certificates, less, wget, Avahi, D-Bus, and the exact SPDX short id for
`gopkg.in/yaml.v3` (dual MIT/Apache-2.0). Also confirm the `siderolabs/zfs`
extension's own license vs. OpenZFS CDDL-1.0 and state both if they differ.

## Out of scope

- No exhaustive transitive license-text bundle (`THIRD_PARTY_NOTICES`) and no
  generator/Make target (decided: static curated file).
- Apps installed at runtime from the app catalog remain the operator's
  responsibility and are not listed.
- No changes to `LICENSE` itself.
