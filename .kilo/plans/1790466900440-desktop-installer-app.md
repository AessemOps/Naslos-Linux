# Desktop installer app for Naslos (macOS / Windows / Linux-AppImage)

Plan ID: `1790466900440-desktop-installer-app`
Status: implementation-ready (decisions locked with the user 2026-09-27)
Implements: a cross-platform desktop app that installs Naslos onto a
user-provided Talos node booted from the Naslos ZFS ISO, creates the first
admin, provisions TOTP 2FA, points the user at first login, and exports a
recovery ZIP.

## 1. Goal

One downloadable app per OS (`.dmg`/`.app`, `.msi`/`.exe`, `.AppImage`) that takes
six inputs and does the whole install end to end, with a progress bar and a
first-login handoff.

Inputs, in order:
1. IP of the Talos machine (user must have booted it from the Naslos ISO —
   the app shows the link and checksum; it does **not** download or write USB).
2. Wanted local domain / appliance name.
3. Wanted admin username.
4. Wanted admin password.
5. (Optional) add the chosen name to the local resolver.

Outputs: a running instance, a 2FA enrolment (QR + secret) shown prominently,
the first-login URL, and a ZIP of the Talos files to keep.

## 2. Decisions locked (from the interview)

- **Images/registry are a non-issue for now** — a future CI builds and publishes
  them. The installer/values must only be *parameterised* by an image repository
  base; do not build images from the app and do not depend on the private
  registry `192.168.1.2:30095`.
- **Time zone is out of scope** (dropped by the user). Talos is UTC-only and the
  chart has no timezone value; do not add one.
- **Domain/resolver**: default `<name>.local`; make the advertised name
  configurable; the app attempts a hosts-file entry with privilege elevation and
  always shows the exact line/record as a fallback.
- **First admin**: reuse the owner-gated `POST /api/users` from inside the
  `naslos-terminal` pod (read the `naslos-proxy` Secret, send `Remote-User` /
  `Remote-Groups` / `X-Naslos-Proxy-Secret`). No new auth surface.
- **Recovery ZIP**: full set including the Talos secrets bundle, with a prominent
  master-credential warning.
- **Repo refactor is in scope**: fold OpenLDAP into `charts/naslos` and add an
  installer values profile so a from-scratch install is fully declarative,
  keeping `deploy-vm.sh` / `make install-vm` working.
- **Desktop stack**: Tauri v2 shell (Rust) + Svelte wizard, with the install
  engine as a standalone Go binary shipped as a Tauri sidecar. The engine also
  runs headless (`naslos-install`) so it can be tested and scripted without the
  GUI. *(Chosen by us after the stack question was dismissed; overridable.)*
- **Linux packaging is AppImage, not Flatpak** (user decision 2026-09-27). The
  app runs unsandboxed on the host, so the resolver write is a plain elevated
  `pkexec`/`sudo` action and there is no Flatpak manifest or `finish-args`.
  Tauri's built-in `appimage` bundle target produces it; distribute that single
  file (plus `.deb`/`.rpm` as a convenience if wanted).

## 3. Non-goals (v1)

- Downloading the Talos ISO or writing a USB stick (show link + checksum +
  Etcher/`dd` pointers).
- Non-amd64 nodes (`docs/spec.md` §1.4 is amd64-only).
- Installing OS-level trust for the self-signed cert is best-effort/optional
  (offered, never required).
- cert-manager + the OVH DNS-01 webhook are **not** installed by the installer;
  real ACME certificates stay a UI action on the Domains page.
- Bundle signing/notarization (unsigned v1 with documented Gatekeeper/SmartScreen
  workarounds); add signing later.
- Multi-node clusters, upgrades, uninstall.

## 4. Architecture

```
desktop/            Tauri v2 app: Rust shell + Svelte wizard (thin)
  src-tauri/        spawns/streams the engine (externalBin sidecar)
  ui/               Svelte 5 + Tailwind wizard (progress + QR + ZIP screen)
  bundle/           per-OS bundle config (AppImage for Linux)
installer/          Go module: the install engine (no GUI)
  cmd/naslos-install        headless CLI, --json-progress for the shell
  internal/preflight        reachability + already-installed checks
  internal/talosconfig      PKI + machine-config generation (machinery)
  internal/talosclient      insecure apply-config, bootstrap, kubeconfig
  internal/k8s              kube client, exec, CRDs, local-path
  internal/helm             chart install (helm.sh/helm/v3) + embedded chart
  internal/bootstrap        admin creation (owner API via exec) + TOTP
  internal/resolver         per-OS hosts-file edit (elevated) + fallback text
  internal/archive          recovery ZIP (incl. secrets bundle + README)
  assets/                   go:embed'd chart, Cilium manifest, local-path, patch
```

Why a Go engine: `api/go.mod` already depends on
`siderolabs/talos/pkg/machinery` (config generation + Talos client),
`k8s.io/client-go` (+ `remotecommand` via `k8s.io/kubectl`), and
`helm.sh/helm/v3`. The engine needs no bundled `talosctl`/`kubectl`/`helm`.

Engine ↔ shell contract: newline-delimited JSON on stdout, e.g.
`{"step":"helm","status":"running","pct":55,"msg":"Installing chart"}` and a
terminal `{"step":"done", ...}` or `{"error":{...}}`. The shell renders the bar
and a collapsible log pane; it can cancel by killing the child.

## 5. Install flow and progress milestones

Input screen validates before starting: IP is IPv4 and the admin password
satisfies Authelia's policy (≥8 chars, upper + lower + number).

| # | Milestone (progress label) | Engine action |
| --- | --- | --- |
| 1 | Checking node | TCP probe `:50000` (Talos maintenance). Warn and offer resume if `:6443` already answers (installed). |
| 2 | Generating Talos config | Render machine config from the embedded patch + inputs; persist the Talos secrets bundle+PKI in the state dir. Skip if resuming with state present (never re-key an installed node). |
| 3 | Installing Talos | `apply-config --insecure`; wait for the API to return; `bootstrap`; wait for node health. |
| 4 | Fetching kubeconfig | Talos `Kubeconfig` RPC → state dir. |
| 5 | Installing cluster storage | Apply embedded local-path-provisioner (pinned), label its namespace `privileged`, patch `local-path` as default StorageClass. |
| 6 | Applying CRDs | Apply the Traefik CRDs (cert-manager CRDs skipped in v1). |
| 7 | Deploying Naslos | Helm install the embedded `charts/naslos` with `values-installer.yaml` + input overrides; wait for api/authelia/openldap readiness. |
| 8 | Creating admin | Read `naslos-proxy`; exec `curl` in `naslos-terminal` → `POST /api/users` (uid, email `<uid>@<domain>`, password, `groups:["naslos_admins"]`); verify with `GET /api/users`. |
| 9 | Setting up 2FA | Exec `authelia storage user totp generate <uid> --issuer <domain>` in `naslos-authelia-0`; parse the `otpauth://` URI. |
| 10 | Adding local name | Attempt hosts entry (elevated); otherwise show the line/record. |
| 11 | Packing recovery ZIP | Build the ZIP (see §6.6) in the Downloads dir. |
| 12 | Done | Show `https://<domain>/authelia`, the admin uid, a large QR + base32 secret, and a save-the-ZIP warning. |

Steps 1–11 are individually resumable from a state file
(`<app-data>/naslos-install/state.json` holding inputs + completed steps).

## 6. Implementation tasks (ordered)

### 6.1 Chart refactor (declarative install path)

1. **Chartify OpenLDAP.** Move `openldap/manifests/{statefulset,bootstrap-job,backup-cronjob}.yaml`
   into `charts/naslos/templates/openldap-*.yaml`, parameterised by the existing
   `openldap.*` values. Keep the services/network policies as-is.
2. **Chart-generate the LDAP Secrets.** Add lookup-guarded generation of
   `naslos-openldap` (`admin-password`, `service-password`) and
   `naslos-openldap-tls` (`ca.crt`, `ldap.crt`, `ldap.key`) to the chart, mirroring
   `templates/tls-secret.yaml` / `templates/proxy-secret.yaml`. Then **replace the
   `fail`** in `templates/authelia-config.yaml` (lines ~27–32) with "use the Secret
   we just generated" so a bare `helm install` needs no external step. Reproduce
   the cert SANs from `openldap/generate-secrets.sh` (`DNS:ldap.<domain>`,
   `DNS:naslos-openldap`, `localhost`, `127.0.0.1`).
3. **Keep `deploy-vm.sh` working.** It currently runs `openldap/generate-secrets.sh`
   + applies `openldap/manifests/*`. After chartification, remove those steps from
   the script (or make them no-ops) so the chart owns OpenLDAP; retain the
   standalone manifests only if something still references them (check `docs/`).
4. **Add `charts/naslos/values-installer.yaml`**: `domain`, `ingress.enabled`,
   `sso.domains`, `traefik` hostPorts, `apps.*`, `networkPolicy` CIDRs,
   `buddy.enabled`, and an **image repository base** (placeholder
   `ghcr.io/aessemops/naslos-*`) so `.Values.<comp>.image.repository` no longer
   hard-codes the private registry. Do not touch `values-vm.yaml`'s tags.
5. **Advertised name = domain name.** `shares.discovery.name` already feeds
   `SMB_DISCOVERY_NAME` (samba) and `SMB_NETBIOS_NAME` (api); `samba/image/avahi-daemon.conf`
   hard-codes `domain-name=local` and the entrypoint rewrites `host-name`. Set
   `shares.discovery.name` = the chosen short name in `values-installer.yaml`.
   Verify `naslos.local`-style discovery still works for a renamed appliance.

### 6.2 Engine preflight, config and lifecycle

6. Preflight: `:50000` reachable; distinguish maintenance vs installed (`:6443`).
7. Machine-config generation with `pkg/machinery/config/generate` +
   `configloader`, starting from `bootstrap/vm/naslos-vm.yaml` with:
   - the user's node IP as the cluster endpoint,
   - the ZFS installer image pinned to the schematic
     `4dd8e3a8b6203d3c14f049da8db4d3bb0d6d3e70c5e89dfcc1e709e81914f63c` / Talos
     `v1.14.1`,
   - the Cilium inline manifest (embed `bootstrap/cilium/cilium.yaml` as the
     render script does),
   - **no** private registry mirror/auth block,
   - the `KubeNodeConfig` / `KubeProxyConfig` / `ResolverConfig` documents and the
     `UnattendedInstallConfig` strip semantics.
   Store the patch template at `bootstrap/installer/naslos-installer.yaml.tmpl`
   and render it in Go. Persist `talosconfig`, `controlplane.yaml` and the Talos
   secrets bundle to the state dir; refuse to regenerate PKI when state exists.
8. Talos client lifecycle: insecure apply-config, wait for reboot, bootstrap
   (treat already-bootstrapped as success), health wait, kubeconfig fetch.
9. K8s/Helm: apply embedded local-path (pin the same `v0.0.26` manifest locally at
   `installer/assets/local-path-v0.0.26.yaml` instead of the raw GitHub URL), the
   Traefik CRDs, then install the embedded chart with `helm.sh/helm/v3`
   (`LoadDir`/`LoadArchive`) + `values-installer.yaml` + overrides, and wait on
   Deployments/StatefulSets.

### 6.3 Admin + 2FA bootstrap

10. Admin create: fetch `naslos-proxy` (`secret`), `remotecommand.Exec` into
    `deploy/naslos-terminal` running the documented `curl` POST (payload from
    `docs/identity-sso.md#deployment--first-admin`), then GET-verify the user.
11. TOTP: exec `authelia storage user totp generate <uid> --issuer <domain>` into
    `statefulset/naslos-authelia` (pod `naslos-authelia-0`); parse the printed
    `otpauth://totp/...?secret=...` URI; render a QR in the UI and show the
    base32 secret in large type. The pod image is distroless but the `authelia`
    binary is on `PATH`, so exec of a specific command works — do not rely on a
    shell. Never copy the config or DB out of the pod.

### 6.4 Resolver + trust (best-effort)

12. Hosts entry per OS, elevated where possible; always surface the fallback:
    - **Linux (AppImage)**: the app is unsandboxed and runs on the host, so write
      `/etc/hosts` directly through an elevated helper (`pkexec` or `sudo`),
      rewriting only its own marked line. If elevation is declined or fails, show
      `0.0.0.0`-style line + copy button and mention router DNS.
    - **macOS**: `/etc/hosts` via an admin prompt (`osascript ... with administrator privileges`).
    - **Windows**: `%SystemRoot%\System32\drivers\etc\hosts` via UAC.
    Only write when the user opts in; never leave a stale entry behind on failure.
13. Optional TLS trust: fetch `naslos-tls` `tls.crt` and offer to add it to the OS
    trust store (best-effort); otherwise tell the user to accept the warning or
    install a real cert later.

### 6.5 Recovery ZIP

14. Build `naslos-recovery-<domain>-<UTCstamp>.zip` in the Downloads dir containing:
    `talosconfig`, `controlplane.yaml`, the Talos secrets bundle, `kubeconfig`,
    `schematic/naslos.yaml`, `ISO.md` (URL + checksum + writing notes), and
    `README.txt` (recovery steps, admin username, TOTP issuer, `https://<domain>/authelia`,
    where `/var/lib/naslos/buddy-identity.json` lives and why it matters — see
    `AI_Handoff.md` gotcha 4). The app must show a prominent
    "this ZIP is a master credential — store it offline" warning. Do **not** put
    the admin password in the ZIP.

### 6.6 Desktop shell and packaging

15. `desktop/` Tauri v2 + Svelte 5 wizard: input form → confirm → progress bar +
    log pane → 2FA/QR screen → ZIP + first-login screen. Reuse the repo's Tailwind
    look. Map engine JSON events to the progress bar.
16. Bundle the engine as a Tauri `externalBin` per target; the app resolves the
    PyInstaller-style suffix via Tauri's sidecar path API.
17. Linux bundle: configure the Tauri `appimage` target (product name, category,
    icon set, AppRun) and build the AppImage plus `.deb`/`.rpm` from the same
    tree. Document the `libfuse2`/`--appimage-extract-and-run` caveat for distros
    without FUSE2.
18. CI (first workflow in the repo): build the engine for darwin/linux/windows
    (amd64), run `tauri build` per OS, produce `.dmg`/`.app`, `.msi`/`.exe`,
    `.deb`/`.rpm`, and the Linux `.AppImage`. (Image publishing is the later CI
    task the user deferred.)

### 6.7 State, resume and errors

19. State file `<app-data>/naslos-install/state.json` (0600): inputs + per-step
    completion + paths to generated files. "Resume" skips completed steps;
    "Start over" requires deleting state and warns that an installed node's PKI
    must not be re-keyed (mirror the `deploy-vm.sh` / `bootstrap-vm` REGEN note).
20. Every failure surfaces the failing step, the captured output, and a retry;
    never leave the user without the log.

## 7. Validation plan

- **Engine unit tests** (`cd installer && go build ./... && go vet ./... && go test -race ./...`):
  machine-config render/goldens, TOTP `otpauth://` parsing, hosts-file edit
  (idempotent, never clobbers unrelated lines), ZIP contents, state-file resume
  logic, progress event encoding.
- **Chart**: `helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-installer.yaml`
  and `helm template` to prove the OpenLDAP secrets/job render on a **first**
  install (lookup returns nothing) and that the `fail` path is gone.
- **Install plumbing**: `make -n install-vm` and a from-scratch `helm install`
  into a throwaway namespace (or kind) to prove OpenLDAP is chart-owned.
- **Live drill on a fresh VM**: boot the ISO at the app's link, run the headless
  engine against it, and check: node Ready, all pods Running, admin login at
  `https://<domain>/authelia` from the app's link, TOTP code from the app accepted
  (**this is the one unproven link — validate early**), SMB login with the same
  password, and the ZIP restores a working `talosctl`/`kubectl` context.
- **Playwright is unaffected** (appliance-side behavior unchanged), but re-run the
  suite after the chart refactor to catch OpenLDAP regressions
  (`cd ui && ./node_modules/.bin/playwright test`).
- Add the `installer` module to `scripts/audit.sh`.

## 8. Early spikes (do these first — they can change the design)

1. **Prove TOTP login after `authelia storage user totp generate`.** Authelia
   4.38+ changed second-factor registration semantics; confirm on the live VM that
   a CLI-generated device lets `https://naslos.local/authelia` accept a code
   without the web enrolment flow. If it does not, fall back to driving the portal
   enrolment and read the elevated-session code from
   `/config/notification.txt` (the notifier is `filesystem`).
2. **Prove machine-config generation from `pkg/machinery` in Go** matches
   `talosctl gen config` output for the installer patch (in particular the Cilium
   inline manifest and the `UnattendedInstallConfig` strip), plus insecure
   `apply-config` and `bootstrap` via the Go client.
3. **Prove Helm SDK can install the embedded chart with the subchart `.tgz`s** and
   that CRDs apply cleanly when pushed as objects.

## 9. Docs, spec and credits (same change, per AGENTS)

- `docs/spec.md`: add a `FR-INSTALL` group (node IP, ISO link, domain/resolver,
  admin creation, TOTP handoff, recovery ZIP) and mark anything unimplemented
  `[OPEN]`.
- New `docs/installer-app.md`; update `docs/deployment.md` (declarative path,
  values-installer), `docs/bootstrap.md` (ISO link), `README.md` (feature +
  layout + requirements), `AI_Handoff.md` (new component + open PRs).
- `CREDITS.md`: Tauri v2, the QR library, any new engine dependency, and the
  Linux AppImage tooling/base if a custom `linuxdeploy` step is used; bump
  `Last reviewed`.
- `AGENTS.md`: add the installer module to the commands table if its gates differ.

## 10. Risks / open items

- **TOTP generation semantics** (spike 1) — the only user-visible step that may
  need a different mechanism.
- **Unsigned desktop binaries**: macOS Gatekeeper and Windows SmartScreen will
  warn; document the bypass and plan signing later.
- **AppImage on the host**: it needs FUSE2 on some distros (document
  `--appimage-extract-and-run`), and it inherits the host's glibc — build against
  an old-enough baseline. Running unsandboxed means the `/etc/hosts` write is a
  normal elevation prompt; the fallback (show the line) must still always be
  present.
- **Chartifying OpenLDAP** touches a live install path; the upgrade must keep
  existing Secrets (lookup) and not restart OpenLDAP destructively.
- **Naming drift**: `naslos.local` is referenced in many docs/defaults; renaming
  to `<name>.local` must be driven entirely by values, not string search/replace.

## 11. Suggested branch / commit shape

Branch `feature/desktop-installer` with sequential Conventional Commits, e.g.
`refactor(chart): fold openldap into the umbrella chart`,
`feat(installer): talos config + lifecycle engine`,
`feat(installer): admin + totp bootstrap`, `feat(desktop): tauri wizard`,
`build(desktop): appimage + per-os bundles`, `docs: installer app`.
No `master` pushes; open a PR against `master`.
