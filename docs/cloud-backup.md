# Cloud backup (S3-compatible)

Naslos cloud backup streams a ZFS dataset into an S3-compatible bucket as
separately encrypted chunks plus a signed manifest (FR-REP-11/12/13). It is a
**ZFS-stream** backup, not a file sync: the restore target is a new dataset with
the source's snapshots. Each destination is independent; the same dataset can be
backed up to several buckets.

The bucket never sees plaintext or a dataset key. What it holds is exactly what
Buddy would hold: sealed chunks (XChaCha20-Poly1305, per-link DEK wrapped to the
NAS KEK) and a manifest signed with the NAS Ed25519 identity. Cloud credentials
are only needed to move bytes.

## What it is not

- Not a file-level sync: rsync-style synchronization and provider-native backup
  APIs remain `[OPEN]` (FR-REP-09).
- Not zero-knowledge against the NAS: an owner who holds the identity file, the
  KEK and (for raw chains) the dataset key can decrypt. Buddy remains the
  zero-knowledge path. See [dataset-encryption.md](dataset-encryption.md).
- Not a substitute for the config export or the installer recovery ZIP. The
  identity/KEK backup below is separate and mandatory.

## Supported providers

| Provider | Endpoint example | `region` | Path style | Conditional create | Notes |
| --- | --- | --- | --- | --- | --- |
| Backblaze B2 (S3 API) | `https://s3.us-west-004.backblazeb2.com` | `us-west-004` | off (virtual-host) or on if the bucket name conflicts | **No** (B2 returns 501 and the probe records it) | Use an application key scoped to the bucket. B2 is the reference low-cost target. |
| Amazon S3 | `https://s3.eu-central-1.amazonaws.com` | `eu-central-1` | off | Yes | An IAM user with `s3:ListBucket`, `s3:GetObject`, `s3:PutObject`, `s3:DeleteObject` on the bucket; no bucket-wide admin. |
| Cloudflare R2 | `https://<account-id>.r2.cloudflarestorage.com` | `auto` | off | Yes (S3-compatible) | An R2 API token scoped to one bucket. |
| MinIO / other gateways | `http://192.168.1.50:9000` | anything accepted by the server (e.g. `us-east-1`) | **on** (usually) | Yes (MinIO) | Private endpoint: see the recipe below; plaintext needs an explicit acknowledgement. |

`POST /api/cloud/destinations/{id}/test` is the capability oracle: it lists the
bucket, then writes and deletes a random probe object with `If-None-Match: *`.
The response is `{ok, category?, conditionalCreate}`. `conditionalCreate` is
`false` when the provider cannot conditionally create (B2 today) or when the
probe could not tell. The flag is stored on the destination and changes the
chunk-write path from "create-or-compare" to "HEAD + PUT + byte compare".

## Bucket setup

Create a dedicated bucket, for example `naslos-backups`, and a prefix for this
NAS (`host1`). The destination fields are:

```
name:      offsite-b2
endpoint:  https://s3.us-west-004.backblazeb2.com
region:    us-west-004
bucket:    naslos-backups
prefix:    host1
pathStyle: false
enabled:   true
```

The API writes only under `<prefix>/<source>/…`. A least-privilege policy for
Amazon S3 (adapt for other providers):

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": ["s3:ListBucket"],
      "Resource": "arn:aws:s3:::naslos-backups",
      "Condition": {"StringLike": {"s3:prefix": ["host1/*"]}}
    },
    {
      "Effect": "Allow",
      "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
      "Resource": "arn:aws:s3:::naslos-backups/host1/*"
    }
  ]
}
```

B2 application keys cannot express an object prefix; scope the key to the
bucket and treat the bucket as dedicated. Enable bucket versioning or an
object-lock/immutability policy if the provider supports it: Naslos deletes old
lineages and sweeps unreferenced chunks, so a versioned bucket is the extra
ransomware/accident guard.

## Credentials

The API writes static credentials to a Secret in the dataset-key namespace
(`datasetEncryption.keysNamespace`, default `naslos-keys`):

```
naslos-cloud-cred-<destination id>
  accessKeyId
  secretAccessKey
  sessionToken        (optional)
  labels: app.kubernetes.io/managed-by=naslos-api
```

- The create/update body may carry `accessKeyId` + `secretAccessKey`
  (+ optional `sessionToken`). A blank field keeps the stored value; WP-1 has
  no clear-token flag.
- To manage the Secret yourself, create it (any name starting with
  `naslos-cloud-`) and pass `credentialsSecret`. Naslos then never writes or
  deletes it; the destination is `credentialsManaged: false`. Switching a
  destination from managed inline credentials to an operator Secret deletes the
  old `naslos-cloud-cred-<id>` Secret after the new record is saved (unless the
  operator reuses that exact name).
- Responses and `/api/system/config/export` expose only `hasAccessKey`,
  `hasSecretKey` and `hasSessionToken`. Deleting a destination deletes its
  Secret only when Naslos created it and the managed-by label is still present.

The API must be able to read Secrets in that namespace; the existing dataset-key
RBAC covers it. No new cluster role is required.

## Network policy and private endpoints

The chart renders a dedicated egress policy (`naslos-api-cloud-egress`) when
`networkPolicy.cloudEgress` is true (default). Public endpoints are opened on
TCP 443 to `networkPolicy.cloudEgressCIDRs` (default `0.0.0.0/0`; narrow it to
the provider's ranges if you want).

The API does not trust the network alone. It resolves the endpoint at connect
time and only dials addresses the policy allows; redirects and proxies are
disabled. The pod CIDR, service CIDR, the node IP, loopback, link-local
(including `169.254.169.254`), CGNAT and IPv6 ULA are always refused.

### MinIO on the LAN (HTTP) recipe

```yaml
# values override
networkPolicy:
  cloudPrivateEndpoints:
    - cidr: 192.168.1.50/32
      port: 9000
```

1. Run MinIO (or another gateway) on a LAN host reachable from the node.
2. Create a bucket, e.g. `naslos-backups`.
3. Add the destination with `endpoint: http://192.168.1.50:9000`,
   `region: us-east-1`, `pathStyle: true`, `allowInsecureHttp: true`, and the
   MinIO access/secret keys. The acknowledgement is audited
   (`cloud/insecure_http_acknowledged`).
4. `POST /test` must return `{ok:true, conditionalCreate:true}` (MinIO supports
   `If-None-Match`).

A Service DNS name inside the cluster (`*.svc`) resolves to the service CIDR and
is always refused; use the host's LAN address. `http` is refused for public
hosts, and an unlisted private host fails with `blocked-address` even when the
NetworkPolicy would allow it.

## Running a backup

From the UI (**Backups → Cloud**) or the API:

```bash
curl -sk -X POST https://naslos.local/api/cloud/send \
  -H 'content-type: application/json' \
  -d '{"dataset":"tank/media","destination":"<id>","keepLineages":2}'
# {"jobId":"…","status":"started"}
```

The job:

1. Checks the identity, credentials, dataset and mount state (a dataset mounted
   only inside a pod is refused, like Buddy).
2. Snapshots the source as `naslos-cloud-<token>-<UTC>` (one token per
   destination, so retention never crosses destinations).
3. Sends raw (`zfs send -w`) when the source is encrypted; otherwise a plain
   stream. Incrementals follow the newest published GUID still present locally.
4. Seals and uploads each chunk under a fresh generation, publishing the signed
   manifest last. An interrupted link resumes when both snapshots and their
   GUIDs still exist.
5. Prunes old lineages after a successful publish (`keepLineages`, default 2)
   and prunes local `naslos-cloud-` snapshots for that destination.

`forceFull: true` throws away an interrupted resume link (its state and the
snapshot that link created) and starts a new full lineage. A deterministic
preflight failure (the manifest/stream cap) also abandons that link instead of
leaving state a later run would silently reuse; an agent/transport error while
estimating is retryable and keeps the resume state.

One job per `(destination, source)` runs at a time; a second one is `409`. Jobs
are in memory; a restart loses progress but not a published chain. Resume state
lives on the state PVC (`CLOUD_SEQUENCES_DIR`); it holds the in-flight chain DEK
and nothing needed to read a published backup.

## Restore and verify

`POST /api/cloud/restore` always creates a **new** dataset. The target must not
exist. The job walks the signed chain from its full link to the requested link,
LISTs every chunk before the first receive, streams each link once, and asserts
the target snapshot GUID after every receive. `chain` is the link id from
`GET /api/cloud/chains`; omitted means the newest restore point's full sequence.

An explicit `chain` restores that older restore point even when newer links
exist; the stale-anchor/"withholding" guard applies only to the implicit newest
restore point. `keepPartial` keeps only the fully applied links: a failed first
full leaves no dataset (ZFS discards it), so there is nothing to keep; the job
error states how many links were kept.

A raw chain (an encrypted source) needs the dataset key:

- the source dataset's key Secret is used automatically when this NAS still has
  it (the usual case); or
- supply the 64-hex key as `keyHex` (write-only) when restoring on a fresh NAS.

Without a key the request is `412`. The replica key Secret for the target is
created create-only; a pre-existing Secret is refused. A failed restore
destroys the partial dataset unless `keepPartial: true` is set.

`POST /api/cloud/verify` has two modes:

- `structure` (default): LIST plus signed manifests plus DEK unwrap. No object
  bodies are downloaded; every missing or wrongly-sized chunk is reported.
- `content`: additionally downloads each chunk, decrypts it and compares its
  SHA-256 with the manifest. This costs one full egress of the protected range.

Both return `missingIndices` and `badIndices` keyed by chain, and the job fails
when either is non-empty. Neither writes to ZFS.

## Retention and sizing

- `keepLineages` (1–100, default 2) is the number of full-lineage restore
  points kept per source. A new full starts after `CLOUD_FULL_EVERY` links
  (default 30) or when incrementals reach the size of the base full.
- Deletion is lineage-safe: an old lineage is deleted only when a newer lineage
  has a published full; links are deleted newest-first and the manifest before
  its chunks. Unreferenced chunks older than 24 h are swept.
- Resume uses an append-only JSON-lines ledger (one record per chunk, fsynced)
  and is compacted away when the manifest publishes. A torn final line is
  ignored; a corrupt earlier line fails the attempt closed.
- A link's manifest carries one entry per ~1 MiB chunk. The send preflight
  refuses a stream whose manifest is estimated above 128 MiB and any single
  link above the agent's 1 TiB receive cap, so an unrestorable chain is never
  uploaded.
- Object count is roughly `stream bytes / 1 MiB` per link. Use a bucket without
  an object-count quota.

## Disaster recovery

The bucket is useless without the NAS identity and KEK: the chunks are encrypted
and the manifests are signed. Key escrow is deliberately not implemented in
WP-1 (no dataset-key escrow; no KEK server). Back them up **before** you need
them, offline, and test a restore.

### Export the identity and KEK (do this now)

```bash
# The identity file contains the Ed25519 private key, the public key and, on
# older installs, the KEK.
kubectl -n naslos exec deploy/naslos-api -- \
  cat /var/lib/naslos/buddy-identity.json > buddy-identity.json.backup
chmod 600 buddy-identity.json.backup

# The KEK Secret (buddy.kekSecret when set, otherwise naslos-buddy-kek).
kubectl -n naslos get secret naslos-buddy-kek -o yaml > naslos-buddy-kek.backup.yaml
chmod 600 naslos-buddy-kek.backup.yaml
```

Store them on offline media in a different location from the bucket. Losing them
makes every cloud and Buddy backup unrecoverable. The KEK Secret and the
identity's `kek` field must agree; never rotate one without the other.

### Export the dataset key for a raw chain

An encrypted source's key is in `naslos-keys`; the raw backup needs it to
restore onto a fresh NAS. Export the key Secret for the source's encryption
root (never put it in the bucket or a ticket):

```bash
# Find the key Secret for the source's encryption root (GET /api/datasets shows
# the root; the Secret's annotation repeats it).
kubectl -n naslos-keys get secret -l app.kubernetes.io/component=dataset-key \
  -o jsonpath='{range .items[*]}{.metadata.annotations.naslos\.local/dataset}{"\t"}{.metadata.name}{"\n"}{end}'
kubectl -n naslos-keys get secret <secret-name> -o yaml > dataset-key.yaml
chmod 600 dataset-key.yaml
# For a one-off restore, copy the 64-hex value from the Secret's `key` field.
```

The exact Secret name is deterministic
(`dataset_encryption::secret_name_for_dataset`). Treat the export as the data
itself: anyone with it and the bucket can decrypt that dataset.

### Fresh-NAS restore runbook

1. Install Naslos with the same chart/version family. Do **not** create the
   source dataset.
2. Restore the identity and KEK:
   - Apply `naslos-buddy-kek.backup.yaml` (or set `buddy.kekSecret`/
     `buddy.kekImport` per the Buddy runbook), then place
     `buddy-identity.json.backup` on the API's state PVC as
     `/var/lib/naslos/buddy-identity.json`.
   - Restart the API and check `GET /api/buddy/identity` shows the same public
     key/fingerprint as before. If it does not, stop: a new identity cannot read
     the old bucket.
3. Re-create the destination (the id will differ; the chain signatures do not
   depend on it), using the same endpoint/bucket/prefix and credentials. `POST
   /test` must pass.
4. `GET /api/cloud/chains?destination=<id>&source=<dataset>` must list the old
   chains. If it is empty, check the prefix, credentials, identity and clock.
5. Restore the newest full (or a chosen link) into a new dataset with `keyHex`
   for a raw chain. Then verify `content` on that chain.
6. Compare the restored snapshot content with the source (or the last known
   manifest `plainBytes` and file checksums).
7. Only after a successful restore should you reuse the old dataset names.

### If the API/node is lost but the bucket and keys are safe

Any NAS with the identity file/KEK and the dataset key can read the bucket; the
destination id and node name are not part of the signed material. Restoring the
same bucket into a different Naslos install is supported by design.

A re-created destination gets a new server id and therefore a new local
sequence anchor. Before the next send, the API seeds that anchor from the
highest signed sequence it finds in the bucket for the source, so a new id can
never overwrite an existing link path or reuse a sequence. When the local anchor
is absent, the bucket is the source of truth.

## Drill

`scripts/drill-cloud-backup.sh` is a committed, operator-run drill. It stands up
a throwaway MinIO in `naslos-apps`, creates a destination, sends a throwaway
dataset, interrupts the API mid-send, resumes, restores to a new dataset,
verifies both modes, and greps logs/responses for credential leaks. It is
**not** run against production by CI and must not be pointed at a real bucket
without confirming the cleanup step. Read the header before running it.

## Limits and open items

- Raw-key escrow for a fresh NAS without the original key is not implemented
  (the key export above is the documented path).
- There is no clear-session-token flag in WP-1; overwrite the Secret or supply
  a new access key to change it.
- The `buddy-receiver` image also links `object_store` (the cloud code is not
  feature-gated yet); it never instantiates a destination.
