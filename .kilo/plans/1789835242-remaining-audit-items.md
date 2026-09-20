# Remaining audit items — plan

All Highs and NAS-010 are closed; these are the open Medium/Low items from
`docs/AUDIT-2026-09-19-REPORT.md` §8, plus the not-yet-run Batch 6 coverage.
Baseline: branch `audit/full-2026-09-19` @ `016683b`, live at helm revision 24
(api/ui `0.1.0-r6`, agent `0.1.0-r3`). One branch, one commit per item, pushed;
`master` untouched.

Order is by value/risk: L9 and L5/L6 are code-only; L2 and M11 rebuild an image;
M3/M6/M4 change the live platform; Batch 6 is read-only coverage.

## 1. AUDIT-L9 — `.Release.Namespace` + `values.schema.json`

**1a. Namespace.** 44 references to `.Values.namespace` across 15 templates
(`grep -rn '\.Values\.namespace' charts/naslos/templates`). Replace every one
with `.Release.Namespace` so the chart can be installed under any release name
and namespace, then delete the `namespace: naslos` value.

- Files: `agent-daemonset.yaml`, `agent-token.yaml`, `api-deployment.yaml`,
  `authelia-config.yaml`, `buddy-secret.yaml`, `ingress.yaml`, `ingress-api.yaml`,
  `namespace.yaml`, `nfs-daemonset.yaml`, `proxy-secret.yaml`,
  `samba-daemonset.yaml`, `terminal.yaml`, `tls-secret.yaml`,
  `traefik-middleware.yaml`, `ui-deployment.yaml`.
- Watch the non-`metadata.namespace` uses: the forwardAuth address
  (`traefik-middleware.yaml:19`), the LDAP URL (`authelia-config.yaml:115`), the
  agent base URL default (`api-deployment.yaml:137`), the UI `wait-for-api`
  hostname (`ui-deployment.yaml:37`) and the `NASLOS_NAMESPACE` env
  (`api-deployment.yaml:132`). All become `.Release.Namespace`, which resolves
  correctly under `helm -n naslos`.
- `namespace.yaml` still creates the release namespace (adopted by Helm in
  `deploy-vm.sh`); keep it but name it `.Release.Namespace`.
- `values.yaml`: delete the `namespace` key; update the Helm-values table in
  `docs/deployment.md` (the `namespace | naslos` row) to "release namespace
  (`-n naslos`)".

**1b. Schema.** Add `charts/naslos/values.schema.json`: a JSON Schema for the
top-level keys (`api`, `agent`, `ui`, `shares`, `openldap`, `authelia`,
`ingress`, `auth`, `traefik`, `buddy`, `terminal`, `notifications`,
`storage`, `namespace` removed). Types plus a few enums/patterns (severity
values, event names, image repository strings). Deliberately **no**
`additionalProperties: false` — the subcharts add their own keys — so the schema
validates types without blocking legitimate values. Verify with `helm lint` and a
negative test (e.g. `--set api.replicas=abc` must fail).

Verification: `helm lint`, `helm template -n naslos` and a second render with
`-n other` (no `naslos.` DNS strings left), `make install-vm`, E2E 35 passed.

## 2. AUDIT-L5/L6 — log injection and integer conversions

- L5: gosec G706 on user/pod/namespace names in `log.Printf`. Add a small
  `sanitizeLogField` (strip control characters/newlines, cap length) and use it
  where request-derived values are logged (`server/users.go`,
  `server/pods.go`, `server/websocket.go`, `identity/*`).
- L6: G115 narrow conversions in buddy size/mode math (`buddy/store.go`,
  `buddy/quota.go`, `cmd/buddyctl`). Bound-check before converting; add table
  tests for the boundaries.
- Verification: `go vet`, `go test`, `gosec` count drops, no behaviour change.

## 3. AUDIT-L2 — the UI image runs nginx as root

- Move to a non-root nginx: listen on `8080`, `pid /tmp/nginx.pid`, temp paths in
  `/tmp`, `runAsUser: 101`, `runAsNonRoot: true`, read-only root filesystem with
  emptyDir mounts for cache/run. Update the Service `targetPort` and the
  `wait-for-api` probes accordingly.
- Verification: image builds, pod starts as 101, UI serves, E2E 35 passed.

## 4. AUDIT-M11 — Svelte/Vite dev advisories

- `npm audit` full reports 11 advisories (1 high) in the build chain: Svelte
  ≤5.55.6 (SSR/spread XSS), Vite/esbuild, `cookie` via `@sveltejs/kit`.
  `--omit=dev` is clean, so this is tooling, not the shipped bundle.
- Bump Svelte and Vite (CR-08/CR-31; the xterm → `@xterm` move can ride along),
  rebuild the UI, re-run `npm audit`, `svelte-check` and the suite.
- Note: the current build already warns about a Svelte/kit version mismatch
  (`untrack`/`fork`/`settled` not exported), so this bump also cleans that up.

## 5. AUDIT-M3 residual — Authelia's configuration lives in a ConfigMap

**LDAP half done at revision 41.** The subchart has no Secret-backed mount for
`configuration.yml` (only `configMap.existingConfigMap`), so the whole file
cannot move. But the LDAP bind password can: values.yaml now sets
`configMap.authentication_backend.ldap.enabled: true` (the missing piece in the
reverted attempt — without it no `AUTHELIA_AUTHENTICATION_BACKEND_LDAP_PASSWORD_FILE`
renders) plus `password.secret_name`/`path`, with the Secret under
`secret.additionalSecrets` mounted at `path: naslos-openldap` (the mount is
`/secrets/<path>` while the env var is `/secrets/<secret_name>/<path>`, so they
only line up when `path` is the Secret name). `authelia-config.yaml` no longer
writes `password`. `scripts/audit.sh` asserts the env var, the mount and the
absence of an inline password. The remaining residual (`jwt_secret` in the
ConfigMap) is documented in the report as Low; it needs a subchart Secret-backed
config mount or a chart-owned Authelia pod.

## 6. AUDIT-M6 — namespace Pod Security is `privileged`

Needs a design decision, not a one-liner. Options, cheapest first:

1. **Split:** move `agent`, `samba`, `nfs`, `terminal` (the hostNetwork/hostPath/
   privileged workloads) to a `naslos-privileged` namespace; set `naslos` to
   `baseline`. Requires the Services the API/UI use to be cross-namespace
   (`naslos-agent.naslos-privileged.svc`, etc.) and the IngressRoutes updated.
2. **Keep one namespace, tighten what can be tightened:** set the namespace to
   `baseline` and give only the four privileged pods explicit
   `pod-security.kubernetes.io/enforce=privileged` labels — but PSA labels are
   namespace-scoped, so this only works per-namespace; the four would need their
   own namespace anyway (option 1).
3. **Document and accept**, with the privileged set enumerated.

Recommend option 1 in a dedicated window; it is the largest remaining change.

## 7. AUDIT-M4 — no NetworkPolicy, and flannel does not enforce one

**Policy layer done.** `charts/naslos/templates/networkpolicy.yaml` renders a
namespace default-deny plus per-workload ingress allows (agent `:9090` API-only,
API, UI, Authelia, OpenLDAP `636`, Samba, NFS) and a namespace egress policy
(cluster DNS + cluster CIDR, no blanket Internet). Gated on
`networkPolicy.enabled`; CIDRs in `networkPolicy.*`. `scripts/audit.sh` asserts
the key policies and the agent's API-only selector.

**Enforcement still open — this is what closes M4.** The policies are inert under
flannel (no policy controller), so the privileged hostNetwork agent `:9090` still
answers every pod. Note the agent/samba/nfs are **hostNetwork**, and policy CNIs
do not apply pod-level NetworkPolicy to host-network pods, so `naslos-agent-ingress`
cannot restrict `:9090` by itself. Options, both a window:

- **Enforcement:** install Cilium or Calico (replacing flannel on Talos means
  `cluster.network.cni.name: none` + a reboot) **and** add a node-level rule
  (Talos/nftables or a Cilium host firewall policy keyed to the API pod IP)
  allowing only the API pod to `:9090`; or just the host firewall rule without
  the CNI swap.
- Set `networkPolicy.nodeCIDR`/`ingressPluginsCIDR`/`probeCIDR` (values-vm.yaml
  already sets `192.168.1.0/24`) before enforcing.
- Verification once enforced: from the terminal pod `curl naslos-agent:9090` times
  out; from the API pod it still 401s/works; ingress and DNS unaffected.

## 8. AUDIT-L8 / CR-06 — `go test -race`

The buddy scheduler test races. Fix it, then flip `NASLOS_AUDIT_RACE=1` in
`scripts/audit.sh`'s invocation (the script already supports it) and, if CI is
re-added later, in the workflow.

## 9. Batch 6 — coverage still not run

- **Images/SBOM: done.** `trivy image` on all seven live tags: api/ui/agent are
  **0 HIGH/CRITICAL**; the four trixie images carry 219 base-library findings,
  **all with no Debian fix** (`Fixed in: None`) and none in a service package. The
  trixie Dockerfiles now `apt-get dist-upgrade` so a future security pocket is
  picked up on rebuild. `trivy config` found two real issues, both fixed:
  **DS-0031** (`LDAP_ADMIN_PASSWORD="admin"` baked into the openldap image —
  removed, fails closed) and **KSV-0053** (unused `pods/exec` Role on the
  openldap bootstrap SA — Role/RoleBinding deleted).
- **SAST breadth: done.** `semgrep --config=auto` → 4 findings, all verified
  false positives. `govulncheck` → the 4 accepted AUDIT-H3 advisories; `gosec`
  excludes the two reviewed false-positive rules. `scripts/audit.sh` gates on
  both, failing only on a *new* advisory/rule.
- **Active tests: AV-5…AV-12 all done.** AV-5/AV-6/AV-7/AV-9/AV-10 are Go tests
  in `api/internal/server/active_tests_test.go` (+ live read-only probes).
  **AV-8** ran live: the send/verify/read-back path passes (all 7
  `ui/tests/backups.spec.ts` with `NASLOS_RECEIVER_URL=http://naslos-api:8080`),
  the crypto protections are unit-covered, and the drill proved the
  mount-propagation guard correctly refuses an unmounted dataset. **AV-11**
  (TLS 1.3 only, HSTS, header middleware, forged-header 302) and **AV-12**
  (API rolling restart: one ~1s gap, LDAP login still works) both pass live. See
  the report's Batch 6 active-tests section.
- **Buddy crypto deep-dive: done — no findings.** `envelope.go`, `keys.go`,
  `auth.go` and `store.go` reviewed against the guide; see the report's deep-dive
  section. The asymmetric split, AEAD+AAD binding, DEK wrap, manifest signature,
  replay defence and store path safety are all sound.
- Update the report with a coverage section and fold any new findings in.

## Definition of done

Each item lands as one PR/commit with its verification recorded in the report's
remaining-work table (moving it from "open" to "fixed" with the commit and
revision), and the live instance stays green (`/api/health`, anonymous 302, E2E
35 passed). M10 stays excluded unless the operator changes their mind.
