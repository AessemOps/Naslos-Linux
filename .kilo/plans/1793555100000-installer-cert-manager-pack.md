# Ship cert-manager and the OVH webhook in the install pack

Status: proposed
Owner: installer workstream
Repos: `Naslos-Linux` (pack + chart profile), then `Naslos-Installer` (consumer)

## Problem

`make install-vm` installs three prerequisites before the Naslos release:
Traefik + cert-manager CRDs, cert-manager itself, and the
`cert-manager-webhook-ovh` DNS-01 solver. The desktop installer installs none of
them, so every fresh installer deployment serves the chart's generated
self-signed certificate (`charts/naslos/templates/tls-secret.yaml`), and the
SSL/Domains page cannot issue a publicly trusted certificate even when the
operator supplies OVH credentials in the UI.

`charts/naslos/values-installer.yaml` documented this as deliberate
("cert-manager + the OVH webhook are NOT installed by the installer"). That gap
is now the difference between the two install paths, and it is user-visible.

## Design

### Pack contents (Naslos-Linux)

```
naslos-install-pack-<version>/
  charts/naslos/charts/cert-manager-v1.18.2.tgz          (already vendored)
  charts/cert-manager-webhook-ovh-0.9.17.tgz             (new)
  manifests/cert-manager-crds.yaml                       (new, 6 CRDs)
```

- The cert-manager chart is already a vendored subchart, so the pack carries it
  without a new copy.
- `manifests/cert-manager-crds.yaml` is rendered from that pinned subchart with
  `crds.enabled=true --show-only templates/crds.yaml`. cert-manager keeps its
  CRDs in `templates/`, not `crds/`, so Helm cannot install them implicitly;
  the engine must apply them first, exactly like `make crds`.
- `charts/cert-manager-webhook-ovh-0.9.17.tgz` is the same chart version the
  Makefile pins (`OVH_WEBHOOK_CHART_VERSION`), vendored so the installer needs
  no network for chart download (only image pulls).
- `metadata.json` checksums cover both new files automatically.
- Provenance: `bootstrap/cert-manager/crds.yaml` carries a generation header;
  `make cert-manager-crds` regenerates it, `scripts/audit.sh` fails on drift.
  `CREDITS.md` records the chart and its license.

### Chart profile (Naslos-Linux)

`values-installer.yaml`:

- `ovhWebhook.enabled: true` — the RBAC role that lets the webhook read a
  Domain's credential Secret from `naslos-apps` must exist, or OVH issuing
  fails at solve time with a forbidden read.
- `certManager.enabled` stays `false`: the engine installs cert-manager as its
  own release (same as the Makefile), so enabling the subchart would install it
  twice.

### Engine work (Naslos-Installer, follow-up PR after the pack ships)

New step `cert-manager` between `storage` and `helm`:

1. Apply `manifests/cert-manager-crds.yaml` server-side with force-conflicts
   (idempotent, and it must be in place before any Certificate CR).
2. Helm install/upgrade `cert-manager` from
   `charts/naslos/charts/cert-manager-v1.18.2.tgz` into `cert-manager` with
   `crds.enabled=false`, mirroring `make cert-manager`.
3. Helm install/upgrade `cert-manager-webhook-ovh` from
   `charts/cert-manager-webhook-ovh-0.9.17.tgz` with
   `groupName=ovh.naslos.local` (must equal the API's OVH provider
   `webhook.groupName`), `certManager.namespace=cert-manager`,
   `certManager.serviceAccountName=cert-manager`.
4. Wait for `cert-manager` and the webhook deployments, then record the step in
   state so a resume skips it.

Ordering constraints: after `storage` (the controller needs a StorageClass only
for optional features, but the DNS-01 solver path is ready earlier) and before
`helm` (the Naslos release renders the webhook RBAC and the UI gates its SSL
page on the CRDs).

No operator secret is required at install time: the webhook is credential-less
and OVH API credentials are per-Domain Secrets the UI writes later. A node
without egress to `quay.io`/Docker Hub fails the new step with an image-pull
error, which is the correct signal (the Naslos images come from GHCR anyway).

### Versioning and release order

Naslos `0.2.5` carries the pack change; the installer `0.2.5` consumes it after
that pack is released, per the cross-repo rule. The pack stays backward
compatible for an older installer (extra files are ignored), so no
`formatVersion` change.

## Testing

- `make install-pack`, then `tar tzf` for both new members and `metadata.json`
  checksum entries.
- `helm lint` both profiles and `make -n install-vm` (VM path unchanged).
- `scripts/audit.sh` drift check on the CRDs file.
- Engine: unit test for the step ordering/idempotency, plus a live drill:
  fresh install → `kubectl -n cert-manager get pods` all Ready, SSL page offers
  OVH issuance, and a Domain certificate reaches `Ready=True` with OVH
  credentials.

## Risks

- CRD application on an existing cluster: server-side apply with
  force-conflicts matches `make crds`; a pre-existing cert-manager with
  different CRDs is reconciled to the pinned version.
- Chart sizes: the CRD manifest is ~930 KiB in git, comparable to the vendored
  charts already committed; drift is caught by the audit check.
- Cluster footprint: +2 pods (~100 MiB) on a single node; acceptable on the
  reference hardware and required for trusted certificates.
