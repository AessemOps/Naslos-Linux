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
in [`api/go.sum`](api/go.sum), [`ui/package-lock.json`](ui/package-lock.json),
and [`charts/naslos/Chart.lock`](charts/naslos/Chart.lock); each remains under
its own upstream license. Applications installed at runtime from the app catalog
are third-party software chosen by the operator and are not listed here.

**Last reviewed: 2026-09-27.** Maintenance: update this file whenever a direct dependency changes in
`api/go.mod`, `ui/package.json`, `charts/naslos/Chart.yaml`, or the Dockerfiles.
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
| Alpine Linux | `alpine:3.20` | Alpine (musl MIT, BusyBox GPL-2.0-only, apk-tools GPL-2.0-only) | https://alpinelinux.org/ |
| distroless static | `gcr.io/distroless/static-debian12:nonroot` | Apache-2.0 | https://github.com/GoogleContainerTools/distroless |
| nginx unprivileged | `nginxinc/nginx-unprivileged:1.30.5-alpine` | BSD-2-Clause | https://github.com/nginxinc/docker-nginx-unprivileged |
| Go toolchain | `golang:1.26-alpine` | BSD-3-Clause | https://go.dev/ |
| Node.js toolchain | `node:20-alpine` | MIT | https://nodejs.org/ |

## Go modules — direct (`api/go.mod`)

`agent/go.mod` declares no external modules.

| Component | Version | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| github.com/cosi-project/runtime | v1.16.3 | MPL-2.0 | https://github.com/cosi-project/runtime |
| github.com/go-git/go-git/v5 | v5.19.2 | Apache-2.0 | https://github.com/go-git/go-git |
| github.com/go-ldap/ldap/v3 | v3.4.8 | MIT | https://github.com/go-ldap/ldap |
| github.com/gorilla/websocket | v1.5.4-0.20250319132907-e064f32e3674 | BSD-2-Clause | https://github.com/gorilla/websocket |
| github.com/siderolabs/talos/pkg/machinery | v1.14.0 | MPL-2.0 | https://github.com/siderolabs/talos |
| golang.org/x/crypto | v0.57.0 | BSD-3-Clause | https://github.com/golang/crypto |
| gopkg.in/yaml.v3 | v3.0.1 | MIT AND Apache-2.0 | https://github.com/go-yaml/yaml |
| helm.sh/helm/v3 | v3.18.5 | Apache-2.0 | https://github.com/helm/helm |
| k8s.io/api | v0.33.3 | Apache-2.0 | https://github.com/kubernetes/api |
| k8s.io/apimachinery | v0.33.3 | Apache-2.0 | https://github.com/kubernetes/apimachinery |
| k8s.io/cli-runtime | v0.33.3 | Apache-2.0 | https://github.com/kubernetes/cli-runtime |
| k8s.io/client-go | v0.33.3 | Apache-2.0 | https://github.com/kubernetes/client-go |

Indirect Go dependencies (~165 modules) are recorded in [`api/go.sum`](api/go.sum);
each is under the license published by its own module.

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
| @sveltejs/adapter-auto | ^3.0.0 | MIT | https://github.com/sveltejs/kit |
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
| Prometheus | 25.0.0 | Apache-2.0 | https://github.com/prometheus/prometheus |
| Alertmanager (via Prometheus chart) | 25.0.0 | Apache-2.0 | https://github.com/prometheus/alertmanager |
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
| actions/setup-go | v5 | MIT | https://github.com/actions/setup-go |
| actions/setup-node | v4 | MIT | https://github.com/actions/setup-node |
| actions/setup-python | v5 | MIT | https://github.com/actions/setup-python |
| actions/upload-artifact | v4 | MIT | https://github.com/actions/upload-artifact |
| actions/download-artifact | v4 | MIT | https://github.com/actions/download-artifact |
| docker/login-action | v4.6.0 | Apache-2.0 | https://github.com/docker/login-action |
| docker/setup-buildx-action | v4.4.1 | Apache-2.0 | https://github.com/docker/setup-buildx-action |
| docker/build-push-action | v7.4.0 | Apache-2.0 | https://github.com/docker/build-push-action |
| softprops/action-gh-release | v2 | MIT | https://github.com/softprops/action-gh-release |
| govulncheck | latest (CI) | BSD-3-Clause | https://github.com/golang/vuln |
| gosec | latest (CI) | Apache-2.0 | https://github.com/securego/gosec |
| gitleaks (CLI) | v8.30.1 | MIT | https://github.com/gitleaks/gitleaks |

## Ported & adapted code

Some Naslos code is ported or adapted from another project. The upstream
license and copyright notice apply to the derived parts.

| Component | What was used | License (SPDX) | Upstream |
| --- | --- | --- | --- |
| ddns-updater | Dynamic DNS provider configurations (`api/internal/providers/builtin/*.yaml` for `duckdns`, `dynu`, `noip`, `freedns`, `namecheap`, `desec`, `spdyn`, `selfhostde`, `dynv6`, `digitalocean`, `godaddy`, `porkbun` and OVH DynHost), the DNS-resolution detection model, and the `digitalocean`/`godaddy`/`porkbun` driver logic | MIT | https://github.com/qdm12/ddns-updater |
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
| samba, samba-common-bin, samba-vfs-modules, smbclient | GPL-3.0-or-later | https://www.samba.org/ |
| libnss-extrausers | GPL-2.0-only AND LGPL-2.1-or-later | https://tracker.debian.org/pkg/libnss-extrausers |
| avahi-daemon, avahi-utils | LGPL-2.1-or-later (GPL-2.0-or-later for some files) | https://github.com/avahi/avahi |
| dbus | AFL-2.1 OR GPL-2.0-or-later | https://gitlab.freedesktop.org/dbus/dbus |
| wsdd2 | GPL-3.0 | https://github.com/Netgear/wsdd2 |
| tini | MIT | https://github.com/krallin/tini |

### nfs image (`nfs/image/Dockerfile`)

| Package | License (SPDX) | Upstream |
| --- | --- | --- |
| nfs-ganesha, nfs-ganesha-vfs | LGPL-3.0 (LGPL-3.0-or-later for some files) | https://github.com/nfs-ganesha/nfs-ganesha |
| nfs-common (nfs-utils) | GPL-2.0-or-later | https://linux-nfs.org/ |
| procps | GPL-2.0-or-later AND LGPL-2.1-or-later | https://gitlab.com/procps-ng/procps |
| tini | MIT | https://github.com/krallin/tini |

### openldap image (`openldap/image/Dockerfile`)

| Package | License (SPDX) | Upstream |
| --- | --- | --- |
| slapd, ldap-utils (OpenLDAP) | OpenLDAP Public License (OLDAP-2.8 style) | https://www.openldap.org/ |
| openssl | Apache-2.0 | https://www.openssl.org/ |
| ca-certificates | GPL-2.0-or-later AND MPL-2.0 | https://tracker.debian.org/pkg/ca-certificates |

### terminal image (`terminal/image/Dockerfile`)

| Package | License (SPDX) | Upstream |
| --- | --- | --- |
| bash | GPL-3.0-or-later | https://www.gnu.org/software/bash/ |
| e2fsprogs | GPL-2.0-only AND LGPL-2.0-only AND BSD-3-Clause AND MIT | https://e2fsprogs.sourceforge.net/ |
| util-linux | GPL-2.0-or-later AND LGPL-2.1-or-later AND BSD-3-Clause AND MIT | https://github.com/util-linux/util-linux |
| procps | GPL-2.0-or-later AND LGPL-2.1-or-later | https://gitlab.com/procps-ng/procps |
| iproute2 | GPL-2.0-or-later | https://git.kernel.org/pub/scm/network/iproute2/iproute2.git |
| iputils | BSD-3-Clause AND GPL-2.0-or-later | https://github.com/iputils/iputils |
| dnsutils (BIND 9) | MPL-2.0 | https://gitlab.isc.org/isc-projects/bind9 |
| curl | curl (MIT-like) | https://github.com/curl/curl |
| ca-certificates | GPL-2.0-or-later AND MPL-2.0 | https://tracker.debian.org/pkg/ca-certificates |
| less | GPL-3.0-or-later OR BSD-2-Clause | https://www.greenwoodsoftware.com/less/ |
| nano | GPL-3.0-or-later | https://www.nano-editor.org/ |
| jq | MIT | https://github.com/jqlang/jq |
| tini | MIT | https://github.com/krallin/tini |

### zsh-terminal image (`zsh-terminal/Dockerfile`)

| Package | License (SPDX) | Upstream |
| --- | --- | --- |
| zsh | MIT (zsh license) | https://www.zsh.org/ |
| zsh-autosuggestions | MIT | https://github.com/zsh-users/zsh-autosuggestions |
| zsh-syntax-highlighting | BSD-3-Clause | https://github.com/zsh-users/zsh-syntax-highlighting |
| bash | GPL-3.0-or-later | https://www.gnu.org/software/bash/ |
| curl | curl (MIT-like) | https://github.com/curl/curl |
| wget | GPL-3.0-or-later | https://gitlab.com/gnuwget/wget |
| jq | MIT | https://github.com/jqlang/jq |
| vim | Vim | https://github.com/vim/vim |
| nano | GPL-3.0-or-later | https://www.nano-editor.org/ |
| htop | GPL-2.0-or-later | https://github.com/htop-dev/htop |
| iotop | GPL-2.0-or-later | http://guichaz.free.fr/iotop/ |
| ncdu | MIT | https://dev.yorhel.nl/ncdu |
| tmux | ISC | https://github.com/tmux/tmux |
| openssh-client | BSD-2-Clause AND ISC | https://www.openssh.com/ |
| ca-certificates | GPL-2.0-or-later AND MPL-2.0 | https://tracker.debian.org/pkg/ca-certificates |
| zfs, zfs-utils (ZFS userspace) | CDDL-1.0 | https://github.com/openzfs/zfs |
| kubectl | Apache-2.0 | https://github.com/kubernetes/kubectl |
| talosctl | MPL-2.0 | https://github.com/siderolabs/talos |

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
