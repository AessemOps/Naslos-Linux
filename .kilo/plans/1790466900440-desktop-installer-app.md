# Desktop installer app for Naslos — two-repo plan

Plan ID: `1790466900440-desktop-installer-app`
Status: implementation-ready (decisions locked with the user 2026-09-27)
Repos involved:
- **`Naslos-Linux`** (this repo) — supplies the declarative install artifacts and a
  versioned **install pack**; gets minimal refactors so a from-scratch install
  needs no developer tooling.
- **`Naslos-Installer`** (`git@github.com:AessemOps/Naslos-Installer.git`, already
  cloned and **empty** on `main`) — owns the desktop app, the Go install engine,
  packaging and release CI.

This plan file lives in Naslos-Linux because the required Naslos-Linux changes
are the blocking part. The `Naslos-Installer` repo gets its own plan/README when
work there starts; §8 below lists exactly what it must contain so the engine can
be built.

## 1. Goal

One downloadable app per OS (`.dmg`/`.app`, `.msi`/`.exe`, Linux `.AppImage`)
that installs Naslos onto a user-provided machine booted from the Talos ISO
(Naslos ZFS schematic), end to end, with a progress bar and a first-login handoff.

Inputs, in order:
1. IP of the machine (user boots it from the Talos ISO — the app shows the
   link and checksum; it does **not** download or write USB).
2. Wanted local domain / appliance name.
3. Wanted admin username.
4. Wanted admin password.
5. (Optional) add the chosen name to the local resolver.

Outputs: a running instance, a 2FA enrolment (QR + secret) shown prominently, the
first-login URL, and a ZIP of the Talos files to keep.

## 2. Decisions locked (from the interview)

- **Installer code lives in `Naslos-Installer`**, not here. Naslos-Linux keeps no
  `desktop/` or `installer/` tree.
- **Artifact hand-off = versioned install pack.** Naslos-Linux CI publishes
  `naslos-install-pack-<version>.tar.gz` as a GitHub release; `Naslos-Installer`
  pins a version, downloads+checksum-verifies it at build time, and `go:embed`s
  it. Repos stay decoupled and builds reproducible.
- **Images/registry are a non-issue for now** — a future CI builds and publishes
  them. Packages must only be *parameterised* by an image repository base; do not
  depend on the private registry `192.168.1.2:30095`.
- **Time zone is out of scope** (dropped by the user). Talos is UTC-only and the
  chart has no timezone value; do not add one.
- **Linux packaging is AppImage, not Flatpak.** The app runs unsandboxed, so the
  resolver write is a normal elevated `pkexec`/`sudo` action; no Flatpak manifest
  or `finish-args`. Tauri's built-in `appimage` target builds it (plus
  `.deb`/`.rpm` as a convenience).
- **Domain/resolver**: default `<name>.local`; make the advertised name
  configurable; the app attempts a hosts-file entry with privilege elevation and
  always shows the exact line/record as a fallback.
- **First admin**: reuse the owner-gated `POST /api/users` from inside the
  `naslos-terminal` pod (read the `naslos-proxy` Secret, send `Remote-User` /
  `Remote-Groups` / `X-Naslos-Proxy-Secret`). No new auth surface.
- **Recovery ZIP**: full set including the Talos secrets bundle, with a prominent
  master-credential warning.
- **Naslos-Linux refactor is in scope**: fold OpenLDAP into `charts/naslos`, add
  `values-installer.yaml`, and add the install-pack build target — keeping
  `deploy-vm.sh` / `make install-vm` working.
- **Desktop stack**: Tauri v2 shell (Rust) + Svelte wizard, with the install
  engine as a standalone Go binary shipped as a Tauri sidecar. The engine also
  runs headless (`naslos-install`) so it is testable and scriptable without the
  GUI. *(Chosen by us after the stack question was dismissed; overridable.)*

## 3. Non-goals (v1)

- Downloading the Talos ISO or writing a USB stick (show link + checksum +
  Etcher/`dd` pointers).
- Non-amd64 nodes (`docs/spec.md` §1.4 is amd64-only).
- Importing the self-signed cert into the OS trust store is best-effort/optional.
- cert-manager + the OVH DNS-01 webhook are **not** installed by the installer;
  real ACME certificates stay a UI action on the Domains page.
- Bundle signing/notarization (unsigned v1 with documented Gatekeeper/SmartScreen
  workarounds); add signing later.
- Multi-node clusters, upgrades, uninstall.

## 4. Two-repo split

### Naslos-Linux (this repo) — declarative install artifacts

| Deliverable | Purpose |
| --- | --- |
| Chart refactor | OpenLDAP folded into `charts/naslos`; lookup-guarded LDAP Secrets; no install-time `fail` |
| `charts/naslos/values-installer.yaml` | Installer profile: public image base, `domain`, advertised name, exposure/ingress, CIDRs |
| `bootstrap/installer/naslos-installer.yaml.tmpl` | Machine-config patch template (IP + no private-registry mirror + Cilium inline) |
| `install-pack/` build + release CI | Versioned `naslos-install-pack-<version>.tar.gz` |
| `docs/installer-contract.md` | Every stable interface the installer depends on |
| Docs/spec/credits updates | FR-INSTALL, README, deployment/bootstrap, AI_Handoff, CREDITS |

Nothing in Naslos-Linux imports or knows about Tauri; the only coupling is the
install-pack format and the contract doc.

### Naslos-Installer (external repo) — the app

```
cmd/naslos-install          headless CLI, --json-progress
internal/preflight          reachability + already-installed checks
internal/installpack        load/verify the embedded pack (metadata + checksums)
internal/talosconfig        PKI + machine-config generation (machinery)
internal/talosclient        insecure apply-config, bootstrap, kubeconfig
internal/k8s                kube client, exec, CRDs, local-path
internal/helm               chart install (helm.sh/helm/v3) from the pack
internal/bootstrap          admin creation (owner API via exec) + TOTP
internal/resolver           per-OS hosts-file edit (elevated) + fallback text
internal/archive            recovery ZIP (incl. secrets bundle + README)
desktop/                    Tauri v2 app
  src-tauri/                spawns/streams the engine (externalBin sidecar)
  ui/                       Svelte 5 + Tailwind wizard (progress + QR + ZIP)
  bundle/                   per-OS bundle config (AppImage for Linux)
scripts/fetch-install-pack.sh   download + sha256 verify + extract to installpack/
installpack/                generated (gitignored); go:embed target
```

Why a Go engine: `siderolabs/talos/pkg/machinery` (config generation + Talos
client), `k8s.io/client-go` (+ `remotecommand` via `k8s.io/kubectl`) and
`helm.sh/helm/v3` cover everything; the engine bundles no `talosctl`/`kubectl`/`helm`.

Engine ↔ shell contract: newline-delimited JSON on stdout, e.g.
`{"step":"helm","status":"running","pct":55,"msg":"Installing chart"}` and a
terminal `{"step":"done",...}` or `{"error":{...}}`. The shell renders the bar and
a collapsible log pane; it cancels by killing the child.

## 5. The install pack (format)

`naslos-install-pack-<version>.tar.gz`, produced by `make install-pack` and
published by CI:

```
metadata.json            naslosVersion, chartVersion, talosVersion (v1.14.1),
                         schematicId, isoUrls{metal-amd64}, sha256 of each member
charts/naslos/           umbrella chart incl. charts/*.tgz subcharts
charts/naslos/values-installer.yaml
machine-config/naslos-installer.yaml.tmpl
cilium/cilium.yaml
manifests/local-path-v0.0.26.yaml
schematic/naslos.yaml
```

ISO URL (from the pinned schematic in `bootstrap/vm/naslos-vm.yaml`,
`4dd8e3a8b6203d3c14f049da8db4d3bb0d6d3e70c5e89dfcc1e709e81914f63c`, Talos
`v1.14.1`):
`https://factory.talos.dev/image/4dd8e3a8b6203d3c14f049da8db4d3bb0d6d3e70c5e89dfcc1e709e81914f63c/v1.14.1/metal-amd64.iso`

The installer refuses to run against a pack whose `talosVersion`/`schematicId`
do not match what it expects, and records `naslosVersion` in the recovery README.

## 6. Install flow and progress milestones

Input validation before start: IP is IPv4; admin password satisfies Authelia's
policy (≥8 chars, upper + lower + number).

| # | Milestone | Engine action |
| --- | --- | --- |
| 1 | Checking node | TCP probe `:50000` (maintenance). Warn/offer resume if `:6443` answers (installed). |
| 2 | Generating Talos config | Render machine config from the pack template + inputs; persist PKI + secrets bundle in the state dir. Skip when resuming; never re-key an installed node. |
| 3 | Installing Talos | Insecure `apply-config`; wait for the API; `bootstrap`; wait for node health. |
| 4 | Fetching kubeconfig | Talos `Kubeconfig` RPC → state dir. |
| 5 | Installing cluster storage | Apply embedded local-path, label its namespace `privileged`, patch `local-path` as default StorageClass. |
| 6 | Applying CRDs | Apply the Traefik CRDs (cert-manager CRDs skipped in v1). |
| 7 | Deploying Naslos | Helm install the pack's chart with `values-installer.yaml` + overrides; wait for api/authelia/openldap readiness. |
| 8 | Creating admin | Read `naslos-proxy`; exec `curl` in `naslos-terminal` → `POST /api/users` (uid, `<uid>@<domain>`, password, `groups:["naslos_admins"]`); verify with `GET /api/users`. |
| 9 | Setting up 2FA | Exec `authelia storage user totp generate <uid> --issuer <domain>` in `naslos-authelia-0`; parse the `otpauth://` URI. |
| 10 | Adding local name | Attempt the hosts entry (elevated); otherwise show the line/record. |
| 11 | Packing recovery ZIP | Build the ZIP (see §7.5) in the Downloads dir. |
| 12 | Done | Show `https://<domain>/authelia`, the admin uid, a large QR + base32 secret, and a save-the-ZIP warning. |

Steps 1–11 are resumable from `<app-data>/naslos-install/state.json`.

## 7. Implementation tasks

### 7.1 Naslos-Linux — chart and install pack

1. **Chartify OpenLDAP.** Move `openldap/manifests/{statefulset,bootstrap-job,backup-cronjob}.yaml`
   into `charts/naslos/templates/openldap-*.yaml`, parameterised by the existing
   `openldap.*` values. Keep services/network policies as-is.
2. **Chart-generate the LDAP Secrets.** Add lookup-guarded generation of
   `naslos-openldap` (`admin-password`, `service-password`) and
   `naslos-openldap-tls` (`ca.crt`, `ldap.crt`, `ldap.key`), mirroring
   `templates/tls-secret.yaml` / `templates/proxy-secret.yaml`. **Replace the
   `fail`** in `templates/authelia-config.yaml` (~lines 27–32) so a bare
   `helm install` needs no external step. Reproduce the SANs from
   `openldap/generate-secrets.sh` (`DNS:ldap.<domain>`, `DNS:naslos-openldap`,
   `localhost`, `127.0.0.1`).
3. **Keep `deploy-vm.sh` working.** It runs `openldap/generate-secrets.sh` and
   applies `openldap/manifests/*`; after chartification remove those steps (the
   chart owns OpenLDAP) and keep the script as the VM path.
4. **`values-installer.yaml`**: `domain`, `ingress.enabled`, `sso.domains`,
   `traefik` hostPorts, `apps.*`, `networkPolicy` CIDRs, `buddy.enabled`, and an
   **image repository base** (placeholder `ghcr.io/aessemops/naslos-*`) so
   `.Values.<comp>.image.repository` no longer hard-codes the private registry.
   Do not touch `values-vm.yaml`'s tags.
5. **Advertised name = domain name.** `shares.discovery.name` feeds
   `SMB_DISCOVERY_NAME` (samba) and `SMB_NETBIOS_NAME` (api); the samba entrypoint
   rewrites `host-name` and `avahi-daemon.conf` fixes `domain-name=local`. Set
   `shares.discovery.name` to the chosen short name in `values-installer.yaml` and
   verify renamed discovery.
6. **Machine-config patch template** at `bootstrap/installer/naslos-installer.yaml.tmpl`
   derived from `bootstrap/vm/naslos-vm.yaml`: node IP endpoint, ZFS installer
   image pinned to the schematic/`v1.14.1`, Cilium inline manifest,
   `KubeNodeConfig` / `KubeProxyConfig` / `ResolverConfig`, **no** private-registry
   mirror/auth, and the `UnattendedInstallConfig` strip note.
7. **`make install-pack`** + `scripts/build-install-pack.sh`: render the template
   once as a reference, copy chart + `.tgz`s + Cilium + local-path (pin
   `v0.0.26` locally instead of the raw GitHub URL) + schematic, emit
   `metadata.json` with per-member sha256 and the ISO URL, and build
   `dist/naslos-install-pack-<version>.tar.gz`. Add to `scripts/audit.sh`.
8. **Release CI** (`.github/workflows/install-pack.yml`): on tag, run
   `make install-pack`, attach the tarball + checksums to the GitHub release.

### 7.2 Naslos-Linux — contract doc

9. **`docs/installer-contract.md`**: the stable interfaces the installer relies
   on — namespaces; `deploy/naslos-terminal`, `statefulset/naslos-authelia`
   (`naslos-authelia-0`); `naslos-proxy`/`secret`; `POST /api/users` payload and
   the `Remote-User`/`Remote-Groups`/`X-Naslos-Proxy-Secret` headers; Authelia
   `totp generate`; `naslos-tls`; `/var/lib/naslos/buddy-identity.json`; the
   install-pack schema. Any change to these must update the doc and the installer
   in the same release.

### 7.3 Naslos-Installer — engine (new repo)

10. **Scaffold** the module `github.com/AessemOps/Naslos-Installer`; add
    `scripts/fetch-install-pack.sh` (download the pinned release, verify sha256,
    extract into `installpack/`, which is gitignored) and a Makefile target that
    runs it before `go build`/`go test`.
11. **Preflight**: `:50000` reachable; distinguish maintenance vs installed
    (`:6443`).
12. **Config generation** with `pkg/machinery/config/generate` + `configloader`
    from the pack template; persist `talosconfig`, `controlplane.yaml` and the
    Talos secrets bundle; refuse to regenerate PKI when state exists.
13. **Talos lifecycle**: insecure apply-config, wait for reboot, bootstrap (treat
    already-bootstrapped as success), health wait, kubeconfig fetch.
14. **K8s/Helm**: apply the pack's local-path + Traefik CRDs; install the pack's
    chart via the Helm SDK + `values-installer.yaml` + overrides; wait on
    Deployments/StatefulSets.

### 7.4 Naslos-Installer — bootstrap

15. **Admin create**: fetch `naslos-proxy`, `remotecommand.Exec` into
    `deploy/naslos-terminal` with the payload from
    `docs/identity-sso.md#deployment--first-admin`, then GET-verify.
16. **TOTP**: exec `authelia storage user totp generate <uid> --issuer <domain>`
    into `naslos-authelia-0`; parse the printed `otpauth://totp/...?secret=...`;
    render a QR and show the base32 secret in large type. The image is distroless
    but the `authelia` binary is on `PATH`; never rely on a shell and never copy
    the config/DB out of the pod.

### 7.5 Naslos-Installer — resolver, trust, ZIP

17. **Hosts entry per OS**, elevated where possible, always with a fallback:
    - **Linux (AppImage)**: write `/etc/hosts` directly via `pkexec`/`sudo`,
      rewriting only its own marked line.
    - **macOS**: `/etc/hosts` via an admin prompt (`osascript … with administrator privileges`).
    - **Windows**: `%SystemRoot%\System32\drivers\etc\hosts` via UAC.
    Only on opt-in; never leave a stale entry on failure.
18. **Optional TLS trust**: fetch `naslos-tls` `tls.crt`, offer to add it to the
    OS trust store (best-effort); otherwise tell the user to accept the warning.
19. **Recovery ZIP** `naslos-recovery-<domain>-<UTCstamp>.zip`: `talosconfig`,
    `controlplane.yaml`, Talos secrets bundle, `kubeconfig`,
    `schematic/naslos.yaml`, `ISO.md` (URL + checksum + writing notes), and
    `README.txt` (recovery steps, admin username, TOTP issuer, login URL,
    buddy-identity location/importance — see `AI_Handoff.md` gotcha 4, and the
    naslosVersion from the pack metadata). Show a prominent "master credential —
    store offline" warning. Never include the admin password.

### 7.6 Naslos-Installer — desktop shell and packaging

20. **Tauri v2 + Svelte 5 wizard**: input form → confirm → progress bar + log pane
    → 2FA/QR screen → ZIP + first-login screen; map engine JSON events to the bar.
21. **Sidecar**: bundle the engine as `externalBin` per target; resolve via
    Tauri's sidecar path API.
22. **Linux bundle**: configure the `appimage` target (product name, category,
    icons, AppRun) and build AppImage + `.deb`/`.rpm`; document the
    `libfuse2`/`--appimage-extract-and-run` caveat.
23. **Release CI**: build the engine for darwin/linux/windows (amd64), run
    `tauri build` per OS, produce `.dmg`/`.app`, `.msi`/`.exe`, AppImage, `.deb`/`.rpm`.
24. **State/resume/errors** as in §6: `<app-data>/naslos-install/state.json`
    (0600); "Start over" warns that an installed node's PKI must not be re-keyed
    (mirror `deploy-vm.sh` / `bootstrap-vm` REGEN note); every failure surfaces the
    failing step, captured output and a retry.

## 8. Validation plan

- **Naslos-Linux**: `helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-installer.yaml`;
  `helm template` proves OpenLDAP Secrets/Job render on a **first** install and
  the Authelia `fail` is gone; `make -n install-vm`; `make install-pack` produces a
  tarball whose `metadata.json` checksums match. Re-run the Playwright suite
  (`cd ui && ./node_modules/.bin/playwright test`) after the chart refactor.
- **Naslos-Installer**: `go build ./... && go vet ./... && go test -race ./...`
  (config render goldens, install-pack verification, `otpauth://` parsing,
  idempotent hosts edit, ZIP contents, resume logic, progress encoding). Add the
  module to its own audit script.
- **End-to-end live drill** (from the installer): boot the ISO at the pack's link,
  run `naslos-install` headless, and check node Ready, pods Running, admin login
  at `https://<domain>/authelia`, the app's TOTP code accepted (**validate
  early**, spike 1), SMB login with the same password, and the ZIP restoring a
  working `talosctl`/`kubectl` context.

## 9. Early spikes (do first — they can change the design)

1. **Prove TOTP login after `authelia storage user totp generate`.** Authelia
   4.38+ changed second-factor registration semantics; confirm on the live VM
   that a CLI-generated device lets the portal accept a code without web
   enrolment. Fallback: drive portal enrolment and read the elevated-session code
   from `/config/notification.txt` (notifier is `filesystem`).
2. **Prove machine-config generation from `pkg/machinery` in Go** equals
   `talosctl gen config` for the pack template (Cilium inline, `UnattendedInstallConfig`
   strip), and that insecure `apply-config` + `bootstrap` work via the Go client.
3. **Prove the Helm SDK installs the pack's chart with the subchart `.tgz`s** and
   that CRDs apply cleanly as objects.
4. **Prove the Tauri sidecar + release tooling** produces all three OS bundles and
   the AppImage from one CI run.

## 10. Docs, spec and credits (per AGENTS, same change)

- `docs/spec.md`: add `FR-INSTALL` (node IP, ISO link, domain/resolver, admin
  creation, TOTP handoff, recovery ZIP); mark unimplemented parts `[OPEN]`.
- `docs/installer-contract.md` (new, §7.2); update `docs/deployment.md`
  (declarative path, values-installer, install pack), `docs/bootstrap.md` (ISO
  link + schematic), `README.md` (installer app + link to the other repo),
  `AI_Handoff.md` (new component, open PRs, deploy notes).
- `CREDITS.md`: install-pack tooling; the installer repo's CREDITS covers Tauri,
  the QR library and its other dependencies.
- `AGENTS.md`: mention the install-pack target and the cross-repo release order.

## 11. Risks / open items

- **TOTP generation semantics** (spike 1) — the only user-visible step that may
  need a different mechanism.
- **Cross-repo version skew**: the installer must pin a pack version and refuse a
  mismatched `talosVersion`/`schematicId`; the contract doc is the guard.
- **Unsigned binaries**: macOS Gatekeeper and Windows SmartScreen warn; document
  the bypass and plan signing later.
- **AppImage on the host**: needs FUSE2 on some distros (document
  `--appimage-extract-and-run`) and inherits the host glibc (build on an old-enough
  baseline). Unsandboxed means the `/etc/hosts` write is a normal elevation
  prompt, but the fallback line must always be shown.
- **Chartifying OpenLDAP** touches a live install path; upgrades must keep
  existing Secrets (lookup) and not restart OpenLDAP destructively.
- **Naming drift**: `naslos.local` appears in many defaults; renaming must be
  driven by values, not string search/replace.

## 12. Repo/PR shape

- **Naslos-Linux** branch `feature/install-pack`: `refactor(chart): fold openldap into the umbrella chart`,
  `feat(chart): installer values profile`, `feat(install-pack): build + release target`,
  `docs: installer contract + spec FR-INSTALL`.
- **Naslos-Installer** branch `feat/desktop-installer`: `feat(engine): talos config + lifecycle`,
  `feat(engine): admin + totp bootstrap`, `feat(engine): recovery zip + resolver`,
  `feat(desktop): tauri wizard`, `build(desktop): appimage + per-os bundles`,
  `ci: install pack fetch + release`.
- Land the Naslos-Linux pack + contract first (the installer pins it), then the
  installer. Never push to `master`; open PRs against `master`.
