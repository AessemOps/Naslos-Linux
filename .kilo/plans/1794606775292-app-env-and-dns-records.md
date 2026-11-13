# App env vars + automatic app DNS records (CNAME → DDNS)

Two features in one pack. Spans `Naslos-Linux` (API, UI, docs, spec) and
`NaslosCharts` (chart template contract). Implementer: first copy this plan to
`Naslos-Linux/.kilo/plans/1794606775292-app-env-and-dns-records.md` and commit
it with the work. Branches: `feature/app-env-vars` and
`feature/app-dns-records` (Naslos-Linux), `feature/app-env` (NaslosCharts).
Open PRs; never push to master/main.

## Decisions (agreed)

1. **Env mechanism: per-app Secret + `envFrom`.** The API owns Secret
   `naslos-env-<release>` in the app's namespace. Charts reference it with
   `envFrom: [{secretRef: {name: naslos-env-<release>, optional: true}}]` on
   the *main app container(s)* only (not gluetun/postgres sidecars). Values are
   never written to `apps.json` or Helm values. Chart-set `env:` names win in
   k8s, so the API **rejects** user names the chart already sets.
2. **DNS link: opt-in per Domain.** `certs::Domain` gains
   `appRecordsDdnsEntry: Option<String>` (DDNS entry id). Only domains with it
   set get app records; this is the definition of "not the local domain" (the
   primary `.local` domain has no Domain record). Target FQDN is resolved from
   the DDNS entry at reconcile time (`record` `@`/empty → `zone`, else
   `record.zone`); wildcard entries are not selectable.
3. **All exposed apps get a record** (non-empty subdomain on an opted-in
   domain), `localOnly` included.
4. **Conflicts:** only records Naslos created are updated/deleted (ownership
   persisted on the app record). Pre-existing CNAME with the same target →
   adopt as owned. Any other record at the name → leave it, surface a DNS error
   on the app.

Derived decisions:

- Records are written with the **Domain's own DNS credentials** (the
  cert-manager Secret in `naslos-apps`). Supported providers: `ovh` (ZoneDNS
  API keys – always present for certs) and `cloudflare` (`api-token`).
  `rfc2136` / `passthrough` domains cannot enable app records (API 400, UI
  disabled).
- Cloudflare records: `proxied: false`, TTL auto. OVH: TTL 0 (zone default),
  CNAME target written absolute (`fqdn.`), then zone refresh.
- TLS is unaffected (per-domain wildcard cert already covers app hosts,
  `certs.rs:204`).

## Part A — App environment variables

### NaslosCharts (`feature/app-env`)
1. Contract in `README.md`: manifest field `capabilities: [env]`. When set,
   the chart MUST add to each main container
   ```yaml
   envFrom:
     - secretRef: { name: naslos-env-{{ .Release.Name }}, optional: true }
   ```
   and to the pod template annotations
   `naslos.io/env-checksum: {{ .Values.naslosEnvChecksum | default "" | quote }}`.
   `naslosEnvChecksum` is platform-owned (never in schema).
2. Apply to all 33 `apps/*/templates/app.yaml` (bentopdf has no `env:` block –
   still add `envFrom`). Add `capabilities: [env]` to every `naslos-app.yaml`.
3. Bump the patch version in **both** `Chart.yaml` and `naslos-app.yaml` per app.
4. Extend `scripts/check-contract.py`: when `env` capability is declared, the
   `helm template` output must contain the `envFrom` ref and the annotation.
5. Gate: `helm lint apps/<app>` + `helm template <app> apps/<app> -n naslos-app-1`
   for every app; `check-contract.py`.

Safe to merge first: `catalog::Manifest` ignores unknown fields and the
`optional: true` ref is harmless before the API ships.

### Naslos-Linux API (`rust/api`)
1. `catalog.rs`: parse `capabilities: Vec<String>` into `App`; expose it in
   `GET /api/catalog/{name}`.
2. Env handling (new `apps/env.rs` or a section of `apps.rs`):
   - `EnvVar { name, value: Option<String>, secret: bool }`.
   - Validation: name `^[A-Za-z_][A-Za-z0-9_.-]*$`, ≤ 128 chars, unique, not
     prefixed `NASLOS_`; ≤ 100 vars; value ≤ 32 KiB; total < 512 KiB.
   - `Record.env: Vec<EnvMeta { name, secret }>` (names/flags only; serde
     default empty → old `apps.json` still loads).
   - Secret `naslos-env-<release>` in `rec.namespace`, labels
     `app.kubernetes.io/managed-by: naslos`, `naslos.io/app: <name>`. RBAC
     already allows secrets in app namespaces (`rbac-apps.yaml:26-30`).
   - Checksum: sha256 of sorted `name=value` → Helm value
     `naslosEnvChecksum`. Add it to the platform-owned keys stripped from
     client values (`apps.rs:26`, `strip_dataset_values`).
3. Reserved names: add `helm::Client::template_dir` (`helm template` with the
   merged values file). Parse Deployments/StatefulSets/DaemonSets; for
   containers whose `envFrom` references the env Secret, collect `env[].name`;
   a user var in that set fails the job with
   `env var X is set by the chart`. Runs in the job before install/upgrade.
4. Install (`InstallBody` `server/apps.rs:48` → `InstallRequest` →
   `Manager::install` `apps.rs:982`): optional `env: Vec<EnvVar>`. Reject
   non-empty env when the chart lacks the `env` capability (400 in handler).
   Write the Secret **before** `helm install`; delete it if install fails.
5. Upgrade (`UpgradeBody` `server/apps.rs:326` → `Manager::upgrade`
   `apps.rs:1170`): `env: Option<Vec<EnvVar>>`; `None` = unchanged, `Some` =
   full replacement; a `secret` var with `value: null` keeps its stored value.
   Snapshot previous Secret data, write new, `helm upgrade --atomic`; restore
   the snapshot on failure. Resource editor / update modal omit `env`.
6. Chart update to a version without the capability while env is non-empty:
   keep the Secret, record a warning on the app; do not fail.
7. Uninstall (`apps.rs:1423`): delete the Secret after `helm uninstall`.
8. `GET /api/apps/{name}`: add `env: [{name, value, secret}]` read from the
   Secret; `value: null` for secret vars. Not added to `GET /api/apps`.
9. Unit tests: validation, checksum stability, reserved-name extraction from
   rendered YAML, secret preserved on `null`, old `Record` JSON without `env`.

### Naslos-Linux UI
1. New `ui/src/lib/components/EnvVarEditor.svelte` (Svelte 5 legacy syntax):
   rows of name / value / "secret" checkbox, add/remove, client-side name
   validation; stored secrets show placeholder
   `unchanged (leave blank to keep)`.
2. `AppInstallModal.svelte`: collapsible "Environment variables" section after
   `SchemaForm` when the app has the `env` capability; send `env` in POST;
   review lists names (secret values masked).
3. `AppConfigureModal.svelte`: load `installed.env`, render the editor, send
   `env` in `PUT /api/apps/{name}`; show a note instead when unsupported.

## Part B — Automatic app DNS records

### API (`rust/api`)
1. `certs::Domain` (`certs.rs:29`): add `app_records_ddns_entry:
   Option<String>` (`appRecordsDdnsEntry`, serde default). Upsert in
   `server/domains.rs:263` validates: provider ∈ {ovh, cloudflare}, entry id
   exists in the DDNS store, entry `record` not a wildcard.
2. New module `dnsrecords.rs`, trait
   `RecordApi { find(zone, fqdn) -> Vec<ZoneRecord{id, type, target}>,
   create_cname, update_cname(id), delete(id) }` — `find` returns all record
   types so conflicts are detected:
   - `OvhRecords`: `GET /domain/zone/{zone}/record?subDomain=` returns an
     **array of numeric ids** → `GET .../record/{id}` each; `POST` / `PUT` /
     `DELETE`; then `POST .../refresh`. Reuse the request signing from
     `ddns/drivers.rs`. Also fix the existing DDNS bug in
     `OvhDriver::update_zonedns` (`drivers.rs:308-331` reads `first.get("id")`
     from a numeric array → PUT `/record/0`) with a test.
   - `CloudflareRecords`: zone id lookup, `GET dns_records?name=<fqdn>`,
     `POST` / `PATCH` / `DELETE`.
   - Zone discovery: walk the labels of `base_domain` (most specific first);
     first zone the provider recognises wins; cache per reconcile pass.
   - Credentials from the Domain Secret (`naslos-domain-*-creds`): `api-token`
     → Cloudflare; `applicationKey/applicationSecret/consumerKey` +
     `providerConfig.endpoint` → OVH.
   - Mock-HTTP unit tests per provider: create, adopt identical, conflict,
     update target, delete.
3. App state: `Record.dns: Option<DnsState { fqdn, target, provider, zone,
   recordId, owned, status: ok|error|pending, error, updatedAt }>` (serde
   default).
4. `reconcile_app(rec) -> Option<DnsState>` (idempotent):
   - Desired iff subdomain non-empty AND domain has `appRecordsDdnsEntry` AND
     entry exists → CNAME `<sub>.<base_domain>` → entry FQDN. Reject target ==
     owner.
   - Owned record with stale fqdn/target → update (same name) or delete +
     create (name changed).
   - Not desired + owned → delete, clear state.
   - Name held by a non-matching record → `status: error`, no write.
   - Errors never fail install/exposure; they are stored like `apply_route`
     errors.
5. Hooks — `apps::Config`/`Manager` (`apps.rs:436-494`) get an
   `Arc<dyn AppDns>` built in `state.rs` with access to `certs::Store` and
   `ddns::Store`:
   - `install`: after `apply_route` (`apps.rs:~1162`).
   - `set_exposure` (`apps.rs:1379`): keep old `rec.dns`; reconcile after
     `store.upsert`.
   - `uninstall`: delete owned record before `store.delete`.
   - `reconcile_routes` (startup, `apps.rs:1529`): reconcile all apps.
   - Domain upsert (toggle/change entry) and delete: reconcile every app on
     that domain; on disable/delete remove owned records **before** the domain
     and its credentials are removed.
   - DDNS entry update (`server/ddns.rs:110`): reconcile apps on domains that
     reference it. DDNS entry delete (`server/ddns.rs:132`): **409** while a
     Domain references it.
   - Retry: in the catalog refresher loop (`main.rs:201-237`) reconcile apps
     with `dns.status == error`.
6. API surface: `dns` in `GET /api/apps/{name}` and
   `GET /api/apps/{name}/exposure`; `POST /api/apps/{name}/dns/reconcile`
   (owner) for manual retry; `GET /api/domains` returns
   `appRecordsDdnsEntry` and `appRecordsSupported` per domain.

### UI
1. `DomainForm.svelte`: "Create DNS records for apps" toggle + select of
   non-wildcard DDNS entries (label `record.zone`); disabled with a hint for
   unsupported providers or no DDNS entries.
2. Domains page: show the linked target per domain.
3. `ExposureForm.svelte` / `AppDetailDrawer.svelte`: DNS status
   (`fqdn → target`, ok/error, "Retry"). Install modal: hint that a CNAME
   will be created when the chosen domain has app records enabled.
4. `ui/src/routes/dns/+page.svelte`: mark entries used as app-record targets;
   surface the 409 on delete.

## Docs / spec (same PRs)
- `docs/spec.md`: **FR-APP-26** (custom env vars: Secret-backed, chart vars
  reserved, secret masking) and **FR-DNS-09** (opt-in app CNAMEs, ownership /
  adopt / conflict rules, cleanup on uninstall / exposure change / domain
  disable), each with its test.
- `docs/api.md` (install/upgrade `env`, app `env`/`dns`, domain field,
  reconcile endpoint, DDNS delete 409), `docs/app-catalog.md` (env + DNS
  sections; fix "400 on schema violation" drift → job failure),
  `docs/dynamic-dns.md` (app records; OVH ZoneDNS keys), `NaslosCharts/README.md`
  (contract), `README.md` feature list, `AI_Handoff.md` (tags, revision, PRs).
- New direct deps (e.g. `sha2`, a mock-HTTP dev-dep) → `CREDITS.md` +
  `Last reviewed`.

## Order of work
1. NaslosCharts `feature/app-env` PR.
2. Naslos-Linux Part A (API → UI → docs/spec), PR.
3. Naslos-Linux Part B incl. OVH id fix, PR. Both parts touch `Record` /
   `apps.rs`: merge A first, then merge master into B (no force-push).
4. Deploy to the production node with a fresh `IMAGE_TAG` in `values-vm.yaml`
   via `make install-vm`; record tags/revision in `AI_Handoff.md`.

## Validation
- `cd rust && cargo fmt --check && cargo clippy -- -D warnings && cargo test`;
  `cd ui && npm run check && npm run build`; both `helm lint` overlays;
  `sh scripts/audit.sh`.
- Playwright: `ui/tests/apps.spec.ts` (install with env → detail returns it,
  secret masked; edit; reserved name such as `TZ` → job failure) and
  `ui/tests/domains.spec.ts` (toggle, unsupported provider 400, DDNS delete
  409).
- Live env drill: install radarr with `FOO=bar` →
  `kubectl -n <ns> exec deploy/radarr -- env | grep FOO`; edit → pod restarts
  with new value; uninstall → Secret gone.
- Live DNS drill on the OVH domain: enable app records → expose an app →
  `dig CNAME <sub>.<domain>` returns the DDNS host; change subdomain → old
  removed, new created; uninstall → removed; pre-create a conflicting A record
  → app shows DNS error, record untouched.

## Risks / out of scope
- `envFrom` avoids Helm 4 SSA duplicate-env errors; reserved-name check covers
  silent shadowing.
- Env values live in a k8s Secret (cluster-default at-rest protection); schema
  secrets in `apps.json` (finding D19) remain open.
- Out of scope: rfc2136 dynamic updates, per-app A/AAAA, split-horizon DNS,
  overriding chart-managed env vars.
