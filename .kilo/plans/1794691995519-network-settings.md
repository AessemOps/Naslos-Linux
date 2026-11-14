# Host network settings page (`/settings/network`)

## Goal

Add a day-2 network settings section to the Naslos UI that inventories the node's
networking and edits the Talos machine configuration behind it: per-physical-link
addressing (DHCP or static), routes/gateway, MTU, and the global DNS resolvers.
Every change is applied live with a machine-level revert guard and an API-side
watchdog, so a bad edit cannot strand the node.

## Locked decisions

| Decision | Choice |
| --- | --- |
| Scope | Full read/write for node networking, with guardrails |
| Rollback | `--mode=try` (60 s machine guard) + API watchdog (5 min) + saved previous config |
| Confirmation | Auto-confirm on proof: the API proves the new address, then confirms machine-side; no operator click required |
| Hostname | Read-only, with an install-time explanation |
| Links | All physical links editable; no VLAN/bond/bridge documents |
| Link speed | Display only (Talos cannot set speed/duplex) |
| Install path | Unchanged (DHCP + auto hostname); runtime editing documented as the day-2 path |
| UI location | `ui/src/routes/settings/network/+page.svelte`, hub card in Settings, no sidebar entry |
| Cross-repo | None: no installer-contract or `Naslos-Installer` change |

## Constraints found in the repo (do not re-litigate)

- No network write path exists today. The only machine-config mutation in the
  product is `machine.install.image` in `rust/api/src/upgrade/ops.rs:642`.
  Network telemetry is read-only through `rust/api/src/talos.rs:354`.
- `rust/agent` is the wrong place: privileged ZFS/share agent, `FROM scratch`,
  no `ip`/`ethtool`, and SEC-1 forbids general host command access.
- Talos v1.14.5 (`rust/api/Dockerfile:33`) semantics:
  - `patch machineconfig --mode=no-reboot|auto` applies live.
  - `--mode=try` applies and auto-reverts after 60 s unless another config
    update arrives; `--mode=staged` waits for a reboot.
  - Patches merge per document (`kind` + optional `name`) and support
    `$patch: delete` for fields and whole documents.
- Config documents: `LinkConfig` (`addresses`, `routes`, `mtu`, `up`),
  `DHCPv4Config`, `DHCPv6Config`, `ResolverConfig` (`nameservers`,
  `searchDomains`, `hostDNS`), `KubeNodeConfig` (`nodeIP.validSubnets`).
  `HostnameConfig` stays untouched.
- `ResolverConfig.nameservers` overrides DHCP-provided resolvers. Unset means
  inherit. The bootstrap patches only set `hostDNS`
  (`bootstrap/vm/naslos-vm.yaml:111-115`), so any patch must preserve it.
- `KubeNodeConfig.nodeIP.validSubnets` is pinned at install time
  (`bootstrap/vm/naslos-vm.yaml:78-84`,
  `bootstrap/installer/naslos-installer.yaml.tmpl:66-72`). An address on another
  subnet without patching this breaks kubelet node-IP selection.
- `EthernetConfig` has rings/features/WOL only - no speed/duplex.
- The API already has a `kubectl` client (`rust/api/src/kube.rs`) and `nodes`
  get/list RBAC (`charts/naslos/templates/rbac-apps.yaml:233-261`), so it can
  discover the node's current `InternalIP` instead of trusting the frozen
  talosconfig endpoint.
- The API mounts an admin talosconfig
  (`charts/naslos/templates/api-deployment.yaml:228-236`), so it can patch
  machine config without new RBAC.
- Talos API server certs auto-include non-loopback interface IPs;
  `machine.certSANs` adds extras. Use it to keep TLS valid across an address
  change and to provide the confirming config update.
- Traefik binds hostPorts, so ingress follows the node IP automatically; the UI
  URL only works again once the operator re-points DNS or `/etc/hosts`.

## Data flow

```
Browser (Authelia session)
  +- GET  /settings/network ----------> GET /api/network
  |                                       \- talosctl: links/addresses/routes/resolvers/
  |                                          machineconfig (+ sysfs speed read)
  +- POST /api/network/changes -------> validate -> read current config -> save to network.json
                                          -> patch certSANs (union old+new)
                                          -> patch target docs with --mode=try
                                          -> poll talosctl at target IP (<=45 s)
                                              +- match  -> confirm patch (certSANs shrink, no-reboot)
                                              |            -> phase=awaiting, 5 min watchdog
                                              \- no match -> let try-mode revert, phase=reverted
  +- GET  /api/network/changes/{id} --> phase, deadline, diff summary
  \- POST /api/network/changes/{id}/revert -> re-apply saved previous config now
```

Talos is the single source of truth for desired state. The API persists only the
change record and the previous config snapshot (needed for revert).

## API contract (owner/admin routes)

Add to the owner router in `rust/api/src/server/mod.rs:88-281`, before the
`/api/{*rest}` fallback:

```
GET  /api/network                     -> inventory + pending change (if any)
POST /api/network/changes             -> 202 { id, phase }
GET  /api/network/changes/{id}        -> { id, phase, target, deadline, error }
POST /api/network/changes/{id}/revert -> 202
```

`GET /api/network` shape:

```json
{
  "hostname": { "value": "<hostname>", "source": "auto-stable" },
  "resolvers": {
    "inherit": false,
    "nameservers": [{ "address": "<ip>", "protocol": "Do53" }],
    "searchDomains": ["lan"],
    "searchInherit": true,
    "hostDns": { "enabled": true, "forwardKubeDnsToHost": false }
  },
  "primaryLink": "<ifname>",
  "links": [{
    "name": "<ifname>", "kind": "ether", "up": true, "mac": "<mac>", "mtu": 1500,
    "speedMbps": 2500, "duplex": "full",
    "addresses": [{ "address": "<cidr>", "source": "dhcp4|static|link" }],
    "gateway": "<ip>",
    "config": { "addressing": "dhcp|static", "addresses": ["<cidr>"],
                "routes": [{ "destination": "<cidr>", "gateway": "<ip>" }],
                "mtu": 1500 },
    "features": { "wol": ["magic"], "ringRx": 256, "ringTx": 256 }
  }],
  "appliance": { "domain": "<domain>", "lanCidr": "<cidr>" },
  "pending": null
}
```

`POST /api/network/changes` body:

```json
{
  "link": "<ifname>",
  "addressing": "static|dhcp",
  "addresses": ["<ip>/<prefix>"],
  "gateway": "<ip>",
  "mtu": 1500,
  "nameservers": { "mode": "inherit|explicit", "servers": ["<ip>"] },
  "searchDomains": { "mode": "inherit|explicit", "domains": ["lan"] }
}
```

Omitted fields are left unchanged. Reject with `409` when a change is already
pending. Errors use the existing `{"error": "..."}` shape.

## Validation (server-side, no shell input)

- `link` must appear in `talosctl get links` as a physical (`ether`) link, not
  loopback, and not already managed as a VLAN/bond/bridge.
- `addressing` in `{dhcp, static}`. `static` requires 1-4 addresses in CIDR form;
  reject loopback, link-local, multicast, unspecified, `/0`, and prefix lengths
  outside 1-32 (v4) / 1-128 (v6). Reject duplicates.
- `gateway` (when given) must be inside one of the configured subnets, must not
  equal a configured address, and must be a unicast IPv4/IPv6 address.
- `mtu` in `576..=9216`; require an explicit confirmation flag below 1280.
- `nameservers.servers`: 1-3 valid IPs when mode is `explicit`; `inherit` deletes
  the field. `searchDomains.domains`: at most 6 DNS-1123 labels when `explicit`.
- Block a static address outside `KubeNodeConfig.nodeIP.validSubnets` unless the
  same change patches `validSubnets` (done automatically for static changes).
- Refuse any change while a change is pending or a Talos upgrade is in flight.

## Patch construction

Build patches from validated state, never from raw user strings.

1. Static on a link:
   ```yaml
   apiVersion: v1alpha1
   kind: LinkConfig
   name: <ifname>
   addresses: [{ address: "<ip>/<prefix>" }]
   routes: [{ gateway: "<gw>" }]      # omitted when no gateway
   mtu: <mtu>                          # only when changed
   ---
   apiVersion: v1alpha1
   kind: DHCPv4Config
   name: <ifname>
   $patch: delete
   ```
   Also delete `DHCPv6Config <ifname>` if present.
2. DHCP on a link:
   ```yaml
   apiVersion: v1alpha1
   kind: DHCPv4Config
   name: <ifname>
   ---
   apiVersion: v1alpha1
   kind: LinkConfig
   name: <ifname>
   addresses: { $patch: delete }
   routes: { $patch: delete }
   ```
   Keep `mtu` if it was set explicitly.
3. MTU only: `LinkConfig.name/.mtu` only.
4. DNS: `ResolverConfig` carrying only `nameservers` and/or `searchDomains`, so
   `hostDNS` is preserved. `inherit` sends `{ $patch: delete }` for the field.
5. Cross-subnet static: add
   ```yaml
   apiVersion: v1alpha1
   kind: KubeNodeConfig
   nodeIP: { validSubnets: ["<cidr>", "<existing>"] }
   ```
6. Legacy migration: if the stored config still carries
   `machine.network.interfaces[].interface == <ifname>` (the bootstrap patches
   use the legacy block), the first edit must add the documents above and delete
   the legacy list entry in the same patch (`machine.network.interfaces`
   merge-deletes by `interface:` key).
7. certSANs: replace `machine.certSANs` with
   `union(existing, <old primary ip>, <new ip>)` via a JSON patch, and use a
   second patch that drops the old IP as the confirming config update.

## Tasks

### T0 - Spike: pin the real Talos shapes (do this first)

Against the live node via a trusted pod, capture and append the findings here
(never commit node IPs or hostnames):

```bash
talosctl get links -o json
talosctl get addresses -o json
talosctl get routes -o json
talosctl get resolvers -o json
talosctl get ethernetstatus -o json
talosctl get machineconfig -o yaml
talosctl get machineconfig v1alpha1 -o json
talosctl read /sys/class/net/<if>/speed
talosctl read /sys/class/net/<if>/duplex
```

Decide from the output:
- whether legacy `machine.network.interfaces` survives in the stored config
  (drives patch rule 6);
- whether re-applying identical content confirms try-mode (if not, the
  certSANs-shrink confirm is mandatory);
- the exact JSON paths for address/route/resolver status.

### T1 - Spec

`docs/spec.md`: new `### 3.12 Host network configuration (FR-NET)` after 3.11
with MUSTs for inventory (FR-NET-01), guarded writes (FR-NET-02), rollback
(FR-NET-03), hostname read-only (FR-NET-04), and install-path divergence
(FR-NET-05). Add the routes to 4.2 and an acceptance row to 7.

### T2 - Talos client extensions (`rust/api/src/talos.rs`)

- Add `run_at(endpoint, args)` overriding `-e`/`-n` for a single call.
- Readers: `links()`, `addresses()`, `routes()`, `resolvers()`,
  `machine_config()` (multi-doc plus parsed documents), `link_speed(ifname)`,
  `link_duplex(ifname)`, `ethernet_status(ifname)`.
- Writers: `patch_config(patch_yaml, mode)`, `replace_config(yaml, mode)` using
  `talosctl patch machineconfig -p @file --mode=<mode>`.
- Introduce a narrow trait (mirroring `crate::certs::Reconciler`) so the change
  state machine is unit-testable without a node.

### T3 - Network module (`rust/api/src/network/`)

Follow the `notifications`/`certs` store pattern:

- `config.rs` - typed view of the machine config: per-link addressing, routes,
  MTU, resolver mode, hostname source, pending legacy migration.
- `validate.rs` - the rules above, pure functions.
- `patch.rs` - patch builder (pure, string output).
- `changes.rs` - `Manager` with `Mutex<State>`, load/save through
  `chartsrepo::write_atomic` to `NETWORK_STATE`
  (default `/var/lib/naslos/network.json`, mode 0600).
  State: `pending { id, phase, link, target, previous_config,
  previous_cert_sans, created_at, deadline, error }`.
- Phases: `applying` -> `verifying` -> `awaiting` -> `applied` | `reverted` | `failed`.
- `watchdog.rs` - `tokio::spawn` task; on startup `resume()` re-reads the store
  and either continues the deadline or reverts immediately if it has passed.
- Endpoint resolution: pending target IP -> else node `InternalIP` from
  `kube::Client` (cached at most 30 s) -> else the configured endpoints.

### T4 - Handlers, routes, state

- `rust/api/src/server/network.rs` with `network_get`, `change_post`,
  `change_detail_get`, `change_revert_post`.
- Register in `rust/api/src/server/mod.rs` next to the `/api/system` routes.
- `AppState` (`rust/api/src/state.rs`): add `network: Arc<network::Manager>`,
  built in `from_env` from `NETWORK_STATE`, and in the test constructor.
- Use `super::zfs::read_json` for the 1 MiB body cap.

### T5 - Chart wiring

- `charts/naslos/templates/api-deployment.yaml`: add
  `NETWORK_STATE: {{ .Values.api.networkStateFile | default "/var/lib/naslos/network.json" }}`
  next to `NOTIFICATIONS_CONFIG`.
- Add `api.networkStateFile` to `values.yaml` with a comment. No new RBAC, no
  `values.schema.json` change for a plain string.

### T6 - UI

- New `ui/src/routes/settings/network/+page.svelte` (legacy Svelte syntax,
  `.card`/`.input`/`.btn`, direct `fetch`, local interfaces - same style as
  `notifications`).
- Sections: pending-change banner (phase, countdown, "Revert now"), hostname
  (read-only plus install-time note), DNS resolvers (inherit vs explicit), link
  cards (telemetry plus inline edit form), appliance summary (domain, LAN CIDR,
  links to `/domains` and `/dns`).
- Apply flow: summary modal with the explicit warning that the browser loses the
  page when the address changes and that DNS or `/etc/hosts` must be re-pointed;
  block applying while a change is pending.
- `ui/src/routes/settings/+page.svelte:2-16`: add a `Network` hub card.
- Poll `GET /api/network/changes/{id}` every 2 s while a change is pending.

### T7 - Tests

- Rust unit tests: every validation rule; patch builders for dhcp-to-static,
  static-to-dhcp, mtu-only, dns inherit/explicit, cross-subnet `KubeNodeConfig`,
  legacy migration; state-machine transitions with a fake Talos trait (success,
  unreachable, revert, resume-after-restart, 409).
- Playwright `ui/tests/network.spec.ts`: page renders link/hostname/DNS data,
  invalid CIDR/gateway produce a client error with no POST, hub card navigates.
  No mutating network test may run by default - gate any apply test behind an
  explicit opt-in env var that CI and the live suite never set.

### T8 - Docs

- `docs/api.md`: routes, request/response shapes, `NETWORK_STATE` env var.
- `docs/deployment.md`: "Day-2 network changes" (page usage, confirm window, DNS
  re-pointing, `KubeNodeConfig` subnet rule, MTU + Cilium caveat) and the
  statement that the install path stays DHCP.
- `docs/bootstrap.md` and `docs/architecture.md`: replace "the API does not
  reapply machine-config patches" with the new network-settings exception.
- `docs/operations.md`: recovery runbook (`talosctl apply-config` from the saved
  snapshot; console/ISO fallback) - no node identifiers.
- `README.md` feature list; `AI_Handoff.md` after deployment.

## Post-apply side effects the implementation must handle

- MTU change: Cilium detects device MTU at agent start, so on confirm restart the
  CNI (`kubectl -n kube-system rollout restart ds/cilium`, already permitted by
  `rbac-apps.yaml:106`) and report that in the change result.
- Address change: kubelet, Cilium and DDNS converge on their own; the operator
  must re-point DNS or `/etc/hosts` to the new address.
- Resolver change: CoreDNS inherits host resolvers; with `hostDNS.enabled` the
  change takes effect through the host resolver - no chart restart needed.

## Validation gates

```bash
cd rust && cargo fmt && cargo clippy -D warnings && cargo test
cd ui && npm run check && npm run build
helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml
helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-installer.yaml
make -n install-vm
sh scripts/audit.sh
cd ui && ./node_modules/.bin/playwright test tests/network.spec.ts
```

Live drill before hand-off, with console/IPMI access arranged: MTU 1500 to 1400
and back; switch one non-primary link to static and confirm the watchdog path;
change the resolver list and revert; confirm `GET /api/network` reflects each
state.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| New address unreachable from the API pod | `--mode=try` reverts at 60 s without API involvement; the watchdog is the second layer |
| API pod restarts mid-change | Pending state is on the PVC; `resume()` finishes or reverts on boot |
| Try-mode confirmation needs a real diff | Confirm by shrinking `machine.certSANs` (drop the old IP); fall back to `apply-config -f` of the target config |
| Legacy `machine.network.interfaces` vs documents | T0 decides; migration happens in the first edit patch |
| Cross-subnet static breaks kubelet | `KubeNodeConfig.nodeIP.validSubnets` patched in the same change |
| MTU change desyncs Cilium | CNI restart on confirm; below 1280 requires extra confirmation |
| Operator stranded without the page | Pre-apply warning, try-mode plus watchdog, documented console/ISO recovery |
| Node identifiers leaking | Never commit node IPs or hostnames in this plan, docs, tests, or the PR |

## Out of scope

- Hostname changes (renames the Node and breaks local-path PV node affinity).
- VLAN, bond and bridge documents; `EthernetConfig` writes (rings, offloads,
  WOL); link speed/duplex.
- Static IP, DNS or MTU in the installer wizard or install-pack contract.
- Appliance-level chart values (public domain, LAN CIDR, Traefik ports) - shown
  read-only only.
- Multi-node and VIP configuration.
- Any non-network machine-config editing.
