# Third-Party Open-Source Credits

Naslos itself is licensed under the **GNU Affero General Public License v3.0**
(see [`LICENSE`](LICENSE)). It depends on, redistributes, and integrates a large
amount of third-party open-source software. This file credits those projects and
records the license each one is distributed under.

This is an index, not a replacement for the upstream license texts: every
component remains under its own license, and where a component is redistributed
inside a built container image its license and copyright notices travel with it
(see the upstream project's own `LICENSE` / `COPYING` and, for packaged OS
software, `/usr/share/doc/<package>/copyright` in the image).

Scope: direct and notable dependencies plus the platform and base images the
project is built on. The complete transitive sets used at build time are pinned
in [`rust/Cargo.lock`](rust/Cargo.lock), [`ui/package-lock.json`](ui/package-lock.json),
and [`charts/naslos/Chart.lock`](charts/naslos/Chart.lock); each remains under
its own upstream license. Applications installed at runtime from the app catalog
are third-party software chosen by the operator and are not listed here.

**Last reviewed: 2026-10-31.** Maintenance: update this file whenever a direct dependency changes in
`rust/api/Cargo.toml`, `rust/agent/Cargo.toml`, `ui/package.json`, `charts/naslos/Chart.yaml`, or the Dockerfiles.
It also lists the GitHub Actions used by the CI/release workflows.

## Platform & runtime

| Component | Version / Pin | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| Talos Linux | v1.14+ | MPL-2.0 | https://github.com/siderolabs/talos |
| Kubernetes | v1.33.x | Apache-2.0 | https://github.com/kubernetes/kubernetes |
| Cilium | pinned manifest (`bootstrap/cilium/cilium.yaml`) | Apache-2.0 | https://github.com/cilium/cilium |
| containerd | Talos-bundled | Apache-2.0 | https://github.com/containerd/containerd |
| etcd | Talos-bundled | Apache-2.0 | https://github.com/etcd-io/etcd |
| CoreDNS | Talos-bundled | Apache-2.0 | https://github.com/coredns/coredns |
| OpenZFS | via `siderolabs/zfs` extension | CDDL-1.0 | https://github.com/openzfs/zfs |
| `siderolabs/zfs` system extension | `bootstrap/schematic/naslos.yaml` | MPL-2.0 | https://github.com/siderolabs/extensions |

## Base & build container images

| Component | Pin | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| Debian (slim base) | `debian:trixie-slim` | Debian DFSG (mixed per package) | https://www.debian.org/ |
| Alpine Linux | `alpine:3.24` | Alpine (musl MIT, BusyBox GPL-2.0-only, apk-tools GPL-2.0-only) | https://alpinelinux.org/ |
| distroless static | `gcr.io/distroless/static-debian12:nonroot` | Apache-2.0 | https://github.com/GoogleContainerTools/distroless |
| nginx unprivileged | `nginxinc/nginx-unprivileged:1.30.6-alpine-slim` | BSD-2-Clause | https://github.com/nginxinc/docker-nginx-unprivileged |
| Rust toolchain | `rust:1.88-alpine` | MIT OR Apache-2.0 | https://www.rust-lang.org/ |
| Node.js toolchain | `node:24-bookworm-slim` | MIT | https://nodejs.org/ |

## Rust crates — direct (`rust/agent` + `rust/api`)

The privileged agent was ported from Go to Rust (Phase 1 of the Go→Rust plan,
`.kilo/plans/1791069821704-go-to-rust-migration.md`). The Phase 2 API port
(`rust/api`, `naslos-api`) reuses axum, axum-server, clap, hyper-util, serde,
tracing, chrono and ipnet; Phase 3 added the `buddyctl` / `buddy-receiver`
binaries (tar streaming, a PTY for terminal exec). Direct dependencies:

| Component | Version | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| aes-gcm | 0.10 | Apache-2.0 OR MIT | https://github.com/RustCrypto/AEADs |
| anyhow | 1 | MIT OR Apache-2.0 | https://github.com/dtolnay/anyhow |
| base64 | 0.22 | MIT OR Apache-2.0 | https://github.com/marshallpierce/rust-base64 |
| async-trait | 0.1 | MIT OR Apache-2.0 | https://github.com/dtolnay/async-trait |
| axum | 0.8 | MIT | https://github.com/tokio-rs/axum |
| axum-server | 0.7 | MIT | https://github.com/programatik29/axum-server |
| bytes | 1 | MIT | https://github.com/tokio-rs/bytes |
| chrono | 0.4 | MIT OR Apache-2.0 | https://github.com/chronotope/chrono |
| clap | 4 | MIT OR Apache-2.0 | https://github.com/clap-rs/clap |
| ed25519-dalek | 2 | BSD-3-Clause | https://github.com/dalek-cryptography/ed25519-dalek |
| futures | 0.3 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| http-body-util | 0.1 | MIT | https://github.com/hyperium/http-body |
| hyper-util | 0.1 | MIT | https://github.com/hyperium/hyper-util |
| getrandom | 0.2 | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| hickory-resolver | 0.24 | MIT OR Apache-2.0 | https://github.com/hickory-dns/hickory-dns |
| include_dir | 0.7 | MIT | https://github.com/Michael-F-Bryan/include_dir |
| ipnet | 2 | MIT OR Apache-2.0 | https://github.com/krisprice/ipnet |
| ldap3 | 0.12 | MIT OR Apache-2.0 | https://github.com/inejge/ldap3 |
| portable-pty | 0.8 | MIT | https://github.com/wez/wezterm |
| tar | 0.4 | MIT OR Apache-2.0 | https://github.com/alexcrichton/tar-rs |
| libc | 0.2 | MIT OR Apache-2.0 | https://github.com/rust-lang/libc |
| reqwest | 0.12 | MIT OR Apache-2.0 | https://github.com/seanmonstar/reqwest |
| jsonschema | 0.60 | MIT | https://github.com/Stranger6667/jsonschema |
| rustls | 0.23 | Apache-2.0 OR ISC OR MIT | https://github.com/rustls/rustls |
| rustls-pemfile | 2 | Apache-2.0 OR ISC OR MIT | https://github.com/rustls/pemfile |
| serde / serde_json | 1 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_yaml | 0.9 | MIT OR Apache-2.0 | https://github.com/dtolnay/serde-yaml |
| sha1 / sha2 | 0.10 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| ssh-key | 0.6 | Apache-2.0 OR MIT | https://github.com/RustCrypto/SSH |
| subtle | 2 | BSD-3-Clause | https://github.com/dalek-cryptography/subtle |
| tempfile | 3 | MIT OR Apache-2.0 | https://github.com/Stebalien/tempfile |
| thiserror | 2 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| tokio | 1 | MIT | https://github.com/tokio-rs/tokio |
| tokio-util | 0.7 | MIT | https://github.com/tokio-rs/tokio |
| tower / tower-http | 0.5 / 0.6 | MIT | https://github.com/tower-rs/tower |
| tracing / tracing-subscriber | 0.1 / 0.3 | MIT | https://github.com/tokio-rs/tracing |
| Rust toolchain (`rust:1.88-alpine`) | 1.88 | MIT OR Apache-2.0 | https://www.rust-lang.org/ |

Transitive crates are recorded in [`rust/Cargo.lock`](rust/Cargo.lock); each is
under the license published by its own crate project.

## npm packages — direct (`ui/package.json`)

Runtime dependencies:

| Component | Version | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| @xterm/xterm | ^6.0.0 | MIT | https://github.com/xtermjs/xterm.js |
| @xterm/addon-fit | ^0.11.0 | MIT | https://github.com/xtermjs/xterm.js |

Build / development dependencies (not redistributed as packages, but used to
produce the shipped static assets):

| Component | Version | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| @playwright/test | ^1.63.0 | Apache-2.0 | https://github.com/microsoft/playwright |
| @sveltejs/kit | ^2.0.0 | MIT | https://github.com/sveltejs/kit |
| @sveltejs/adapter-static | ^3.0.0 | MIT | https://github.com/sveltejs/kit |
| @sveltejs/vite-plugin-svelte | ^4.0.4 | MIT | https://github.com/sveltejs/vite-plugin-svelte |
| @types/node | ^20.0.0 | MIT | https://github.com/DefinitelyTyped/DefinitelyTyped |
| autoprefixer | ^10.4.0 | MIT | https://github.com/postcss/autoprefixer |
| postcss | ^8.4.0 | MIT | https://github.com/postcss/postcss |
| svelte | ^5.57.1 | MIT | https://github.com/sveltejs/svelte |
| svelte-check | ^4.7.6 | MIT | https://github.com/sveltejs/language-tools |
| tailwindcss | ^3.0.0 | MIT | https://github.com/tailwindlabs/tailwindcss |
| typescript | ^5.0.0 | Apache-2.0 | https://github.com/microsoft/TypeScript |
| vite | ^5.0.0 | MIT | https://github.com/vitejs/vite |

The full transitive tree is pinned in
[`ui/package-lock.json`](ui/package-lock.json).

## Helm chart dependencies (`charts/naslos/Chart.yaml`)

| Component | Version | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| Traefik | 41.6.0 | MIT | https://github.com/traefik/traefik |
| Authelia | 0.11.22 | Apache-2.0 | https://github.com/authelia/authelia |
| cert-manager | v1.18.2 | Apache-2.0 | https://github.com/cert-manager/cert-manager |

## Integrated services

| Component | Pin | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| ntfy (server) | operator-configured; not a chart dependency | Apache-2.0 (GPL-2.0-only for the Android app) | https://github.com/binwiederhier/ntfy |

## CI & release tooling

Pinned GitHub Actions (`.github/workflows/ci.yml`, `release.yml`), all pinned to
immutable commit SHAs, plus the secret scanner. **Not** used: `gitleaks-action`,
whose license is commercial for organization accounts; the MIT `gitleaks` CLI is
used instead.

| Component | Pin | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| actions/checkout | v4 | MIT | https://github.com/actions/checkout |
| actions/setup-node | v4 | MIT | https://github.com/actions/setup-node |
| actions/setup-python | v5 | MIT | https://github.com/actions/setup-python |
| dtolnay/rust-toolchain | stable (1.88) | MIT OR Apache-2.0 | https://github.com/dtolnay/rust-toolchain |
| Swatinem/rust-cache | v2 | LGPL-3.0-only | https://github.com/Swatinem/rust-cache |
| actions/upload-artifact | v4 | MIT | https://github.com/actions/upload-artifact |
| actions/download-artifact | v4 | MIT | https://github.com/actions/download-artifact |
| docker/login-action | v4.6.0 | Apache-2.0 | https://github.com/docker/login-action |
| docker/setup-buildx-action | v4.4.1 | Apache-2.0 | https://github.com/docker/setup-buildx-action |
| docker/build-push-action | v7.4.0 | Apache-2.0 | https://github.com/docker/build-push-action |
| gitleaks (CLI) | v8.30.1 | MIT | https://github.com/gitleaks/gitleaks |
| cargo-audit | 0.22.2 | Apache-2.0 OR MIT | https://github.com/rustsec/rustsec |
| cargo-deny | 0.20.2 | Apache-2.0 OR MIT | https://github.com/EmbarkStudios/cargo-deny |
| Trivy | 0.76.1 | Apache-2.0 | https://github.com/aquasecurity/trivy |

## Ported & adapted code

Some Naslos code is ported or adapted from another project. The upstream
license and copyright notice apply to the derived parts.

| Component | What was used | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| ddns-updater | Dynamic DNS provider configurations (`rust/api/src/providers/builtin/*.yaml` for `duckdns`, `dynu`, `noip`, `freedns`, `namecheap`, `desec`, `spdyn`, `selfhostde`, `dynv6`, `digitalocean`, `godaddy`, `porkbun` and OVH DynHost), the DNS-resolution detection model, and the `digitalocean`/`godaddy`/`porkbun` driver logic | MIT | https://github.com/qdm12/ddns-updater |
| cert-manager | ACME DNS-01 certificates and Issuer/Certificate CRs (chart dependency `cert-manager`, Apache-2.0); the OVH provider's `apiRights` and webhook solver are derived from cert-manager's DNS-01 documentation | Apache-2.0 | https://cert-manager.io/docs/configuration/acme/dns01/ |
| cert-manager-webhook-ovh (aureq) | The OVH DNS-01 webhook (Helm chart + image) that renders the OVH `webhook` solver; `apiRights` text derived from its README | MIT | https://github.com/aureq/cert-manager-webhook-ovh |
| local-path-provisioner (Rancher) | The v0.0.26 `local-path-storage.yaml` manifest, pinned at `bootstrap/local-path/local-path-storage.yaml` and shipped in the install pack as `manifests/local-path-v0.0.26.yaml` (FR-INSTALL) | Apache-2.0 | https://github.com/rancher/local-path-provisioner |

## Bundled OS packages

Debian and Alpine package licenses are governed by each upstream package; the
authoritative copyright file ships in the image at
`/usr/share/doc/<package>/copyright`.

### samba image (`samba/image/Dockerfile`)

| Package | License (SPDX) | Upstream |
| --- | --- | --- |
| samba, samba-common-bin, samba-vfs-modules | GPL-3.0-or-later | https://www.samba.org/ |
| libnss-extrausers | GPL-2.0-only AND LGPL-2.1-or-later | https://tracker.debian.org/pkg/libnss-extrausers |
| avahi-daemon | LGPL-2.1-or-later (GPL-2.0-or-later for some files) | https://github.com/avahi/avahi |
| dbus | AFL-2.1 OR GPL-2.0-or-later | https://gitlab.freedesktop.org/dbus/dbus |
| wsdd2 | GPL-3.0 | https://github.com/Netgear/wsdd2 |
| tini | MIT | https://github.com/krallin/tini |

### nfs image (`nfs/image/Dockerfile`)

| Package | License (SPDX) | Upstream |
| --- | --- | --- |
| nfs-ganesha, nfs-ganesha-vfs | LGPL-3.0 (LGPL-3.0-or-later for some files) | https://github.com/nfs-ganesha/nfs-ganesha |
| tini | MIT | https://github.com/krallin/tini |

### openldap image (`openldap/image/Dockerfile`)

| Package | License (SPDX) | Upstream |
| --- | --- | --- |
| slapd, ldap-utils (OpenLDAP) | OpenLDAP Public License (OLDAP-2.8 style) | https://www.openldap.org/ |
| openssl | Apache-2.0 | https://www.openssl.org/ |

### terminal image (`terminal/image/Dockerfile`)

| Package | License (SPDX) | Upstream |
| --- | --- | --- |
| bash | GPL-3.0-or-later | https://www.gnu.org/software/bash/ |
| busybox (base + `chroot`) | GPL-2.0-only | https://busybox.net/ |
| e2fsprogs | GPL-2.0-only AND LGPL-2.0-only AND BSD-3-Clause AND MIT | https://e2fsprogs.sourceforge.net/ |
| util-linux | GPL-2.0-or-later AND LGPL-2.1-or-later AND BSD-3-Clause AND MIT | https://github.com/util-linux/util-linux |
| iproute2 | GPL-2.0-or-later | https://git.kernel.org/pub/scm/network/iproute2/iproute2.git |
| iputils | BSD-3-Clause AND GPL-2.0-or-later | https://github.com/iputils/iputils |
| bind-tools (BIND 9) | MPL-2.0 | https://gitlab.isc.org/isc-projects/bind9 |
| curl | curl (MIT-like) | https://github.com/curl/curl |
| ca-certificates | GPL-2.0-or-later AND MPL-2.0 | https://gitlab.alpinelinux.org/alpine/aports |
| less | GPL-3.0-or-later OR BSD-2-Clause | https://www.greenwoodsoftware.com/less/ |
| nano | GPL-3.0-or-later | https://www.nano-editor.org/ |
| jq | MIT | https://github.com/jqlang/jq |

## Attribution notes

- SPDX identifiers are given to identify the applicable license; the controlling
  text is always the upstream project's own license file.
- Some projects are multi-licensed or mix licenses across files; where that is
  the case the combination is shown (for example `util-linux`, `e2fsprogs`,
  `avahi`, and `nfs-ganesha`).
- Redistributed components retain their original copyright and license notices.
  Nothing in this file grants additional rights or re-licenses third-party work.
- CI: the `.github/workflows/` workflows use the pinned Actions listed under
  "CI & release tooling" (MIT / Apache-2.0) to run the gate sweep, scan for
  secrets, build and push the images and attach the pack to a release; the pack
  itself bundles only the components listed above.
