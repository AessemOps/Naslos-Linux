# Deployed state — 2026-09-27 (VM before the 2026-11-01 reinstall)

Archived from `AI_Handoff.md`. This describes the 0.1.0 cluster that ran
`make install-vm` on `192.168.1.117` before the node was wiped and reinstalled
with the desktop installer. Kept for the operational history (domains, ZFS
pools, app catalog drill); it is **not** the current deployment.

## Cluster state

On `192.168.1.117`, chart `naslos-0.1.0`, **helm revision 39**:
`naslos-api` **`0.1.0-r33`** (async app lifecycle jobs with progress, FR-APP-18;
exposure base-domain choice, runtime SSO promotion + per-domain Authelia portals,
declarative DNS providers + Dynamic DNS, OVH webhook solver), `naslos-ui`
**`0.1.0-r24`** (job drawer + completion toasts; domain-select exposure form,
Domains-page SSO toggle, cert badge, sign-out), `naslos-agent`
**`0.1.0-r8`**, `naslos-samba`/`naslos-nfs`/`naslos-terminal` **`0.1.0-r3`**,
OpenLDAP per `values.yaml`. **Authelia is a StatefulSet** (`naslos-authelia-0`)
and the `naslos-authelia-sso` fragments ConfigMap exists (API-owned,
`keep`). Talos **v1.14.1** (kernel 6.18.51-talos), Cilium v1.20.2, ZFS pool
`test` (stripe, 79 G) + `test/drill`, `test/naslos-buddy`.

Namespaces: `naslos` (authenticated services), `naslos-privileged`
(agent/samba/nfs/terminal), `naslos-apps` (installed apps, PSA baseline,
`naslos-datasets` RWX PVC), `naslos-apps-priv` (privileged apps), and
**`cert-manager`** (cert-manager v1.18.2 + `cert-manager-webhook-ovh` 0.9.17,
installed by `make install-vm`; deliberately outside the naslos default-deny
policies). Posture is the
**only one**: Traefik v3.7.13 (chart 41.6.0) on hostPort 80/443, Authelia
4.39.24 (chart 0.11.22) at `https://naslos.local/authelia`, no NodePort and no
auth bypass, `admin` in `naslos_admins`.

Domains: `florentinrichard.fr` holds a live production Let's Encrypt wildcard
certificate (`*.florentinrichard.fr` + apex, Secret
`naslos-florentinrichard-fr-tls`) issued through the OVH webhook.

App catalog: official source `https://github.com/AessemOps/NaslosCharts.git`
(public, channels `{Prod: main}`) — catalog lists **10 apps** (Jellyfin, Radarr,
Sonarr, Seerr, FlareSolverr, Prowlarr, qBittorrent, Audiobookshelf, Calibre-Web,
SearXNG). **No app is installed** (the SSO drill's Jellyfin was uninstalled).
Merged into `master`: app catalog (PR #28), Dynamic DNS + providers (PR #29),
third-party credits (PR #30), ddns-updater credits + credits rule (PR #32), the
app-base-domain choice + runtime SSO promotion (PR #35, merged), and the
docs/README currency rules (PRs #36/#37). Merged: `feature/app-install-progress`
(PR #38, async app lifecycle jobs, FR-APP-18). Open: `feature/install-pack`
(PR #39, install pack + chart-owned OpenLDAP, not deployed). Merged:
`feature/domain-provider-api-rights` (PR #33) — declarative per-provider
`apiRights` surfaced by `GET /api/providers` and shown as an info bubble next to
the Domains form's DNS-01 provider selector, plus the fix that makes OVH
certificates actually issue: cert-manager and the `cert-manager-webhook-ovh`
webhook are installed as separate releases in the `cert-manager` namespace
(`make cert-manager`, `make cert-manager-webhook-ovh`), `ovh.yaml` renders the
`webhook` solver, and every OVH credential (application key/secret/consumer key)
is a Secret field read by the webhook. The same PR also adds a sidebar sign-out
control (POST `/authelia/api/logout`) and a self-refreshing certificate status
badge. **Live-drilled on revisions 29–32**: the OVH
Issuer is created and `READY=True`, and `florentinrichard.fr` issued a
production Let's Encrypt certificate (SANs `*.florentinrichard.fr` +
`florentinrichard.fr`, Secret `naslos-florentinrichard-fr-tls`) once the OVH API
token covered the zone. Residual: the token still lacks **DELETE** on
`/domain/zone/florentinrichard.fr/*`, so the webhook's `CleanUp` gets a 403 and
leaves the `_acme-challenge` TXT record in the zone (issuance is unaffected);
add DELETE for the four methods on the zone wildcard so cleanup succeeds. The
pre-refactor revision 55 narrative is archived at
`docs/archive/ai-handoff-log-2026-09.md`.
