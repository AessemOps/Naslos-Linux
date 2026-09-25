# Buddy Backup

Buddy Backup lets one Naslos instance push backups to another Naslos instance —
or to a standalone container with two plain volumes — over a key-authenticated
HTTP API. The receiving side **cannot read what it stores**: the sender encrypts
the stream with a key only it holds, and the receiver keeps bytes it has no way to
open.

That is the whole point. A friend's NAS, a colleague's box or a cheap VPS can hold
your backups without being trusted with your data, and without either side needing
a shared secret, a VPN or an SSH account on the other machine.

## 1. Roles and trust

| | Sender ("A") | Receiver ("B") |
| --- | --- | --- |
| Holds | Ed25519 private key, key encryption key (KEK) | only the sender's **public** key |
| Can do | push, list, restore, prune its own backups, verify signatures | account space, list what a key stored, prune, delete |
| Cannot do | — | read, modify undetectably, or forge any chunk, manifest or request |

Roles are symmetric in the product, not in the protocol: **every instance is both**
a receiver (it can accept pushes from peers) and a sender (it can push to peers).
A receiver is trusted with *availability*, never with *confidentiality* or
*integrity*:

- **Confidentiality** — AES-256-GCM, keyed by a per-chain data key that only the
  sender can unwrap.
- **Integrity** — every chunk is authenticated (GCM tag) and every manifest is
  signed by the sender's Ed25519 key.
- **Provenance** — a manifest carries the sender's key fingerprint and a signature
  over its own contents, so a receiver that rewrites metadata is caught on the next
  restore, even years later.

Two receiver flavours speak the identical protocol:

1. **A Naslos instance** — the receive API is mounted at `/api/buddy/v1/` and the
   chunks land in a ZFS dataset you nominate (chart value `buddy.receiveHostPath`).
2. **The standalone container** — `naslos-buddy-receiver`, two volumes (`/data` for
   chunks, `/config` for authorized keys), no ZFS, no Kubernetes, no database.
   Built from `api/Dockerfile.receiver`.

A sender cannot tell them apart, which is deliberate: the protocol is the
interface.

## 2. Key material

Everything lives in one file per instance — `~/.naslos/buddy-identity.json` by
default, `--identity` (or `BUDDY_IDENTITY`) to point elsewhere — created on first
use by `buddyctl identity`:

```json
{
  "version": 1,
  "name": "naslos-a",
  "publicKey": "ssh-ed25519 AAAAC3Nza… naslos-buddy naslos-a",
  "privateKey": "-----BEGIN OPENSSH PRIVATE KEY-----\n…",
  "kek": "0J8kQ…==",
  "createdAt": "2026-09-14T09:12:44Z"
}
```

- The file is written `0600`, atomically (temp file + rename).
- `publicKey` is an **OpenSSH** Ed25519 key, so `ssh-keygen -t ed25519` also
  produces usable material and what a receiver stores is an authorized-keys line.
- `kek` is the 32-byte key encryption key in base64. **Losing it means losing the
  ability to read the backups**: the receiver holds wrapped data keys it cannot
  unwrap. Back the identity file up like the data itself — ideally somewhere the
  receiver is not, because a receiver holding both the ciphertext and the KEK is
  just a receiver holding the plaintext.
- **The identity file is not encrypted at rest.** It holds the signing key and,
  unless you move it, the KEK. Anyone who can read it (a snapshot of the state
  volume, an arbitrary-file-read in the API, the admin terminal) can both
  impersonate the sender to every buddy *and* decrypt every backup. File
  permissions are `0600`, which is not the same as at-rest encryption.
- **Move the KEK into a Secret.** Set `buddy.kekSecret` (Helm) or `BUDDY_KEK`
  (base64, 32 bytes) and the KEK is read from the environment instead of the
  file; the identity file then exposes only the signing key. The environment
  value takes precedence when both are present, so migrating does not lose access
  to existing backups, and the file's `kek` should be deleted once the Secret is
  in place (the resume state still holds each in-flight chain's DEK in plaintext
  until that push finishes).
- The receiver never sees this file. It stores an authorized-keys line per sender
  and nothing else secret.

## 3. What goes over the wire

### 3.1 Signing (ssh-style, not a shared secret)

Every request carries four headers:

| Header | Content |
| --- | --- |
| `X-Buddy-Key` | key id, ssh-style fingerprint (`SHA256:…`) |
| `X-Buddy-Timestamp` | Unix seconds |
| `X-Buddy-Nonce` | 128-bit random, base64 |
| `X-Buddy-Signature` | base64 Ed25519 signature |

The signature covers this exact string, newline-separated:

```
BUDDY2
<receiver identity>
PUT
/api/buddy/v1/chunks/naslos-a/data?chain=1f3c&index=7
<sha256 hex of the request body>
1757843251
5c2nQw==
```

The second line is the **audience**: the receiver's own identity (`BUDDY_NAME`).
A sender learns it from the read-only `/status` call and signs every later
request with it, so a request captured for buddy A does not verify at buddy B
even if both authorize the same sender key — the cross-receiver replay that
`BUDDY1` allowed. `/status` is the one call a sender may sign with an empty
audience (it cannot know the name yet); the receiver accepts that only there.
Give each receiver a distinct `BUDDY_NAME` (`naslos-a`, `naslos-b`, …): the
default `naslos` is identical everywhere and weakens this binding.

Because the audience, method, full URI (query string and mount prefix included),
body digest, timestamp and nonce are all inside it, a captured request cannot be
replayed twice inside the clock-skew window — nor at a different receiver: the
receiver records each nonce it has
seen, per key, bounded in memory and persisted next to its store (`.nonces`) so a
restart cannot re-arm the window. A captured request cannot be
pointed at another path, another body, or replayed later. The receiver additionally:

- rejects a timestamp more than **5 minutes** from its own clock;
- remembers nonces for twice that window and refuses a repeat;
- burns a nonce only *after* the signature verified, so junk traffic cannot exhaust
  a peer's nonces.

A signature mismatch reports what was signed, because the usual cause is a proxy
that rewrites the path — an error nobody can debug otherwise.

### 3.2 Endpoints (all under `/api/buddy/v1`)

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/enroll` | Bootstrap: a peer presents the receiver's one-time token and its public key. The only unsigned call. |
| GET | `/status` | Free space, bytes stored for this key, quota, sources, last backup, current chain per source |
| GET | `/backups` | Rows: source, chain, kind, created, chunks, stored bytes |
| GET | `/chains/{source}` | Chain list (newest first) with GUIDs, for showing a source's history. Bounded to the newest 500 chains and reports `truncated`/`total` |
| GET | `/sequence/{source}?chain=` | The chains a restore must apply, oldest first, followed through the receiver's GUID index. This is what a restore uses, so its work is proportional to the sequence rather than to the source's whole history |
| GET | `/chunks/{source}?chain=` | Chunk list: index + digest of each stored sealed chunk |
| GET | `/chunks/{source}?chain=&index=` | One sealed chunk (raw bytes) |
| PUT | `/chunks/{source}?chain=&index=` | Upload one sealed chunk (idempotent) |
| GET | `/manifest/{source}?chain=` | A manifest (`chain` omitted = current chain) |
| PUT | `/manifest/{source}` | Publish a signed manifest; refused unless every chunk it lists is present |
| POST | `/prune/{source}` | `{"keep": N}` — keep the newest N chains, delete the rest. Never deletes a chain the newest one descends from: an incremental without its base is not a backup, so a keep-count may retain more chains than asked |

The index travels as a **query parameter** rather than a path segment because a
source can itself contain slashes (`naslos-a/tank/data`); guessing where the source
ends would be a correctness bug waiting to happen.

Owner-facing endpoints on a Naslos instance (not peer-facing):

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/buddy/status` | Receiver state for the UI: free space, peers, stored backups |
| GET/POST/DELETE | `/api/buddy/peers` | List / authorize / revoke peer keys |

`/api/buddy/peers` hands out storage access, so it goes through the owner gate like
every other owner route (SEC-10): the request must carry the proxy-issued shared
secret **and** the identity header, so a client that can reach the API directly
cannot authorize a key by sending a header itself. The peer API is deliberately
*not* gated that way: a peer cannot complete an interactive login, which is the
whole reason it authenticates with its own key.

### 3.3 Pushing and resuming

A push is a loop of independent requests — seal 1 MiB, PUT it, repeat — followed by
the signed manifest. Nothing is one long-lived connection, which is what makes an
interrupted push cheap to resume:

1. The sender writes a **chain state** file (chain id, data key, nonce prefix,
   `0600`) *before* the first chunk leaves, so a crash cannot lose the ability to
   finish.
2. `GET /chunks/{source}?chain=…` returns what already arrived, with a digest per
   chunk.
3. Chunks already present are **verified by digest** and skipped; only the missing
   ones are re-sent.
4. If the source changed since the interruption the digests differ and the resume
   **fails loudly** instead of stitching two versions of the data into one chain.
   Start a new chain (drop `--resume`).
5. After the manifest is published the chain is complete and the state file is
   removed.

## 4. The envelope

The plaintext stream is cut into fixed 1 MiB chunks; each chunk is sealed on its
own. One sealed chunk on disk:

```
"NBC2" magic(4) | plainLen(4, big-endian) | nonce(12) | AES-256-GCM ciphertext + tag
```

- **Key** — a fresh 256-bit data key (DEK) per chain segment.
- **Nonce** — `streamPrefix(6 random bytes) || generation(2, big-endian) ||
  chunkIndex(4, big-endian)`, unique per (key, generation, index) by construction,
  which is the property GCM depends on. The **generation** is bumped whenever a
  resumed push has to re-seal the interrupted tail, so the replacement never
  reuses the partial chunk's `(key, nonce)` pair — the fix for the nonce-reuse
  found in the 2026-09-21 audit (PF-H2). Each chunk's generation is recorded in
  the signed manifest and bound into its AAD.
- **AAD** — `NB2|<source>|<chain>|<generation>|<index>|<plainLen>`, so chunks
  cannot be reordered, swapped between chains, moved between sources, or
  truncated undetected.
- **DEK wrapping** — the DEK is sealed with the owner's KEK and travels in the
  manifest (`AAD: NB2|dek|<source>|<chain>`), so a restore needs the owner's KEK
  and nothing from the receiver.

The manifest carries the chain's metadata, a monotonic **sequence** per
`(receiver, source)`, the chunk list (`index`, plain bytes, sealed bytes, nonce
generation, SHA-256 of the **plaintext**), the wrapped DEK, the sender's key id,
and an Ed25519 signature over all of it. The receiver can verify the signature and
the presence of every chunk; only the owner can check the plaintext digests — which
is exactly the intended asymmetry.

Owners verify the manifest is the one they asked for: a restore binds it to the
requested source (and chain, when one is named) and refuses a sequence older than
the last one this sender published, so a hostile buddy cannot answer with another
source's or a stale but validly signed backup (PF-H3).

> **Protocol version.** The envelope is **v2** (`NBC2`/`NB2`). A v2 sender and a v2
> receiver are required on both ends: an in-flight chain created by an earlier v1
> sender will not verify against a v2 receiver, and vice versa. Finish or discard
> pre-upgrade chains before upgrading a peer, and start a new chain after.

## 5. Operating it

### 5.1 Receiver: a Naslos instance

```bash
# 1. A dataset of its own for received chunks (the API is not a host writer, so
#    this is a deliberate operator step). On the VM layout that is:
zpool list                                   # e.g. pool "test"
zfs create test/naslos-buddy                 # -> /var/mnt/test/naslos-buddy

# 1b. The dataset ownership is fixed automatically: the API deployment runs a
#     short init container (root, reusing the OpenLDAP image because the API image
#     is distroless) that chowns the dataset root to 65532:65532. It only touches
#     the root - the per-key trees inside are created by the API itself - and it
#     skips the work when the ownership is already right.
#     If it cannot (a read-only mount, for instance) the pod fails with the same
#     message you would otherwise get on the first push, so this is the manual
#     fallback:
# chown 65532:65532 /var/mnt/test/naslos-buddy

# 1c. Make sure the dataset is mounted *in the host's mount namespace*, or the
#     API's hostPath will bind the parent dataset's directory instead and your
#     backups will quietly land on the pool root (no quota isolation). On Talos
#     the agent and terminal containers mount /host with
#     mountPropagation: HostToContainer, which is one-way: a `zfs create` run
#     inside a pod mounts the dataset only inside *that pod's* namespace. The
#     host mounts it at boot (the ZFS extension runs `zfs mount -a`), so either
#     reboot the node after creating the dataset, or create it from a
#     host-context process - then verify from a *fresh* pod:
#       kubectl run check --rm -it --image=busybox:1.36 --restart=Never \
#         --overrides='{"spec":{"containers":[{"name":"c","image":"busybox:1.36",
#         "command":["df","-h","/data"],"volumeMounts":[{"name":"d","mountPath":"/data"}]}],
#         "volumes":[{"name":"d","hostPath":{"path":"/var/mnt/test/naslos-buddy","type":"Directory"}}]}}'
#     `Filesystem ... test/naslos-buddy` = correct. Bare `test` = the dataset is
#     not mounted on the host yet: fix that before enabling Buddy Backup, then
#     restart the API deployment so its bind mount picks the dataset up.

# 2. Enable the receive side. buddy.receiveHostPath is required when enabled: the
#    chart refuses to render without it rather than quietly filling the config
#    volume with backups.
helm upgrade --install naslos charts/naslos -n naslos \
  -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml \
  --set buddy.enabled=true \
  --set buddy.receiveHostPath=/var/mnt/test/naslos-buddy \
  --set buddy.name=naslos-b

# 3. Optional: let the first peer authorize itself with a one-time token.
kubectl -n naslos create secret generic naslos-buddy \
  --from-literal=enrollToken="$(openssl rand -hex 16)"
helm upgrade … --set buddy.enrollTokenSecret=naslos-buddy
```

Peers reach this API the same way the UI does — through the ingress, on the
`/api/buddy/v1/` path (Authelia bypasses that prefix; peers authenticate with
their own Ed25519 keys). **No new port and no separate listener**; nothing else
about the deployment changes.

The nginx in front of the UI raises `client_max_body_size` to 8 MB for
`/api/buddy/` specifically, because its default of 1 MB would reject every 1 MiB
chunk with a 413 before it reached the API. That is the kind of thing that only
shows up on the first real push, so the location exists from the start.

### 5.2 Sender: `buddyctl`

```bash
# create (or show) this instance's identity — its public key is what a receiver authorizes
buddyctl identity --name naslos-a

# authorize that key on a receiver that has a token (the token is single use)
buddyctl enroll https://naslos-b --token <token> --sources naslos-a/

# a directory (archived as tar) — no ZFS needed on either side
buddyctl push https://naslos-b --source naslos-a/data --dir /var/mnt/test/data

# a ZFS stream, incrementally, resumable
zfs snapshot test/data@buddy-$(date +%F)
zfs send -w -i @yesterday test/data@buddy-$(date +%F) \
  | buddyctl push https://naslos-b --source naslos-a/data --kind zfs-send

# what the receiver knows: space left, what is stored, when the last backup was
buddyctl status https://naslos-b
buddyctl backups https://naslos-b

# verify without writing anything, then restore
buddyctl restore https://naslos-b --source naslos-a/data --verify-only
buddyctl restore https://naslos-b --source naslos-a/data --dir ./restored

# or straight back into ZFS
buddyctl restore https://naslos-b --source naslos-a/data | zfs recv test/data-restored

# retention, asked of the receiver
buddyctl prune https://naslos-b --source naslos-a/data --keep 7
```

`buddyctl` is built from the repo (`make buddyctl` → `bin/buddyctl`) and needs only
network access to the receiver: it is not a cluster component.

An interrupted push is continued, not restarted:

```bash
buddyctl push $RECEIVER --source naslos-a/data --dir /var/mnt/test/data   # dies at 80%
buddyctl push $RECEIVER --source naslos-a/data --dir /var/mnt/test/data --resume
```

### 5.3 Backing up an instance from itself

An instance can also back *itself* up: the API drives the node's `zfs send`, streams
it into the encrypted push, and can restore it back into ZFS. No `buddyctl`, no
shell, no operator piping:

The owner-facing buddy routes sit behind the same gate as the UI, so a direct
call needs the proxy secret and identity headers, from a source the API's
NetworkPolicy allows (the terminal pod in `naslos-privileged`, or the
Traefik/UI pods) — the old UI NodePort on `:30080` no longer exists. The
examples below run from the terminal pod; in the UI, Traefik injects the same
headers automatically once you are logged in.

```bash
BASE=http://naslos-api.naslos.svc.cluster.local:8080
SECRET=$(kubectl -n naslos get secret naslos-proxy -o jsonpath='{.data.secret}' | base64 -d)
AUTH=(-H "X-Naslos-Proxy-Secret: $SECRET" -H 'Remote-User: admin' -H 'Remote-Groups: naslos_admins')

# 1. Create this instance's key (its public key is what the buddy authorizes).
curl -sX POST "${AUTH[@]}" -H 'Content-Type: application/json' \
  -d '{"name":"naslos-a"}' $BASE/api/buddy/identity

# 2. Back a dataset up. The API answers 202 with a job id immediately and runs
#    the send under a server-owned context (a disconnected client no longer
#    kills it); poll the job for progress and cancel it with DELETE.
curl -sX POST "${AUTH[@]}" -H 'Content-Type: application/json' \
  -d '{"dataset":"test/data","source":"naslos-a/test","receiver":"https://naslos-b"}' \
  $BASE/api/buddy/send                       # → {"jobId":"…","status":"started"}
curl -s "${AUTH[@]}" $BASE/api/buddy/jobs/<jobId>
curl -sX DELETE "${AUTH[@]}" $BASE/api/buddy/jobs/<jobId>   # cancel

# 3. Prove the backup is intact without touching ZFS: it decrypts the stored
#    stream and hashes every chain.
curl -sX POST "${AUTH[@]}" -H 'Content-Type: application/json' \
  -d '{"source":"naslos-a/test","receiver":"https://naslos-b","verify":true}' \
  $BASE/api/buddy/restore

# 4. Restore it (or an older chain) into a dataset.
curl -sX POST "${AUTH[@]}" -H 'Content-Type: application/json' \
  -d '{"source":"naslos-a/test","receiver":"https://naslos-b","dataset":"test/restored"}' \
  $BASE/api/buddy/restore
```

What the send does, in order:

1. Resolves the base: the buddy's manifest records the **GUID** of the snapshot it
   was given, and the node is asked which local snapshot carries that GUID. GUIDs,
   not names, are what make the incremental decision safe - a renamed snapshot still
   counts, and a destroyed one is *known* to be gone.
2. Snapshots the dataset (`buddy-<UTC>-<4 hex>`; the random suffix exists because two
   sends in the same second would otherwise collide).
3. Asks the node for a dry-run size (`zfs send -nP`), streams the send, encrypts and
   uploads it, and only then publishes the signed manifest.
4. If the stream came in short of that estimate by more than the allowance, **no
   manifest is published** and the API says to retry: the chunks stay, the resume
   state stays, and the next attempt continues the same chain with the same snapshot,
   so the buddy skips what it already has.

That last point is worth spelling out, because it is what makes a multi-terabyte
backup over a flaky link tolerable: an interrupted send is resumed, not restarted.
The resume state (chain id, data key, nonce prefix, snapshot pair) lives beside the
identity on the persistent volume, is written `0600` before the first chunk leaves,
and is deleted once the manifest is published.

A restore is usually a **sequence**: the newest backup is normally an incremental,
and ZFS refuses an incremental stream whose base is missing. The API therefore works
out the sequence (the last full send, then each incremental in order, following the
recorded GUID links), applies them into the destination, and refuses up front if a
link is missing - rather than applying half a backup and leaving you to notice.

### 5.4 Receiver: the standalone container

```bash
docker build -f api/Dockerfile.receiver -t naslos-buddy-receiver .

mkdir -p /srv/buddy-data /srv/buddy-config
# The image is distroless and runs as its unprivileged "nonroot" user (uid 65532,
# the same one the API runs as), so hand it both volumes:
sudo chown -R 65532:65532 /srv/buddy-data /srv/buddy-config

docker run -d --name buddy-receiver --restart unless-stopped \
  -p 8484:8484 \
  -e BUDDY_ENROLL_TOKEN="$(openssl rand -hex 16)" \
  -e BUDDY_NAME=backup-nas \
  -v /srv/buddy-data:/data \
  -v /srv/buddy-config:/config \
  naslos-buddy-receiver
```

Senders then use `http://host:8484` exactly as they would use a Naslos instance.
Two volumes are all the state there is: `/data` (ciphertext) and `/config`
(`peers.json`). Keep a copy of `/config` if you want to keep the authorized keys,
or to move them to another receiver.

## 6. The restore drill

A backup nobody has restored is a hope, not a backup. The drill:

```bash
# 1. Push
zfs snapshot test/data@drill
zfs send -w test/data@drill | buddyctl push $RECEIVER --source drill/data --kind zfs-send

# 2. Destroy the source, for real
zfs destroy -r test/data

# 3. Restore it back
buddyctl restore $RECEIVER --source drill/data | zfs recv test/data

# 4. Compare
zfs diff test/data@drill test/data      # no output = identical
```

The negative half of the drill matters as much: corrupt one sealed chunk in the
receiver's store and confirm `buddyctl restore` **fails** with an authentication
error instead of writing damaged data. `TestReceiverRefusesTamperedChunk` in
`api/internal/buddy/buddy_test.go` automates exactly that.

## 7. What is protected, and what is not

**Protected**

- The receiver cannot read a backup, and cannot hand a third party anything
  readable: it holds ciphertext and a wrapped key it cannot unwrap.
- Silent corruption is not possible: a flipped bit, a missing chunk, a swapped
  chunk, a truncated chunk or an edited manifest all fail verification.
- A captured request cannot be replayed. A stolen key file is the only thing that
  can impersonate a sender, and revoking that key (`DELETE /api/buddy/peers?name=…`)
  stops further pushes immediately.
- A sender cannot write outside its own namespace, beyond its quota, or into
  another key's tree: the source name is validated (no `..`, no absolute paths),
  scope is enforced per key, and quota is checked before every chunk is stored.

**Not protected (yet)**

- **Transport privacy.** Encrypting the payload is not encrypting the metadata or
  the connection: an observer sees source names, chain ids, sizes and timing.
  Terminate TLS in front of the API (the ingress you already run) if that matters.
- **Availability.** A receiver can delete backups or refuse requests, and it cannot
  be stopped by cryptography. That is what a second buddy is for.
- **A revoked key's old chunks stay** until they are pruned: revoking stops new
  pushes, it is not a delete. The API says so in its response, deliberately.
- **Scheduling and retention are built in.** Schedules (`hourly|daily|weekly` +
  run-at time, `GET/POST/DELETE /api/buddy/schedules`, stored at
  `BUDDY_SCHEDULES`) run sends through the async job path, prune to `pruneKeep`
  on success and notify via ntfy (`backup_success` / `backup_failure`). The
  `/backups` page manages them, starts manual sends with live progress and
  verifies restores.
- **Retention never breaks a sequence.** `pruneKeep` counts *chains*, but an
  incremental chain needs every chain below it, so the receiver always keeps the
  dependency chain of the newest backup. Asking for `keep=1` on an incremental
  therefore keeps the incremental **and** its base; that is deliberate, because the
  alternative is a backup that verifies as unrestorable long after the schedule
  reported success.
- **A send refuses a dataset the node cannot see.** `zfs send` runs in the host's
  mount namespace; a dataset created from inside a pod is mounted in that pod's
  namespace only, so sending it would capture an empty filesystem and still report
  success. The send checks the agent's `mounted` state first and refuses with the
  fix (`zfs mount <dataset>` on the node, or a reboot) rather than storing a
  worthless chain. Datasets whose mountpoint is `none`/`legacy` are unaffected.
- **Cancelling a send is prompt.** A cancelled job aborts the in-flight chunk
  request (the buddy client's calls are context-bound) and stops between chunks, so
  `DELETE /api/buddy/jobs/{id}` does not wait for a stalled receiver.
- **Enrollment is single use across restarts.** The receiver records the spent
  token in its store (`.enroll-used`), and a peer name cannot be re-keyed without an
  explicit revoke, so a leaked token cannot substitute a key after a restart.
- **tar payloads lose symlinks, devices and xattrs** in v1, and only regular files
  and directories are archived. For filesystems that need all of it, send a ZFS
  stream (`--kind zfs-send`), which preserves everything by construction.
- **A chain is immutable.** A source that changes between push and resume must
  start a new chain; the sender refuses to mix versions rather than guess.

## 8. Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `signature verification failed (signed request: …)` | The path that was signed is in the message: a proxy that rewrites the URL breaks it, and so does a clock more than 5 minutes off. Compare the printed URI with what the receiver serves. |
| `unknown key SHA256:…` | The sending key is not authorized on the receiver. `buddyctl enroll … --token …`, or authorize the public key from `buddyctl identity`. |
| `this request was already used (nonce replay)` | Two identical signed requests: a proxy retrying a request is the usual cause. Signatures are single-use by design. |
| 413 on a chunk, or `client_max_body_size` in a proxy log | A proxy in front limits the body. The UI's nginx needs the `/api/buddy/` location (it is in `ui/nginx.conf`). |
| `mkdir /var/lib/naslos/buddy/SHA256_…: permission denied` | The receive dataset is not writable by the API's user. The API runs as uid 65532 and the `fix-receive-dataset-ownership` init container normally handles this; if it could not, `chown 65532:65532 /var/mnt/<pool>/naslos-buddy` on the node (hostPath volumes ignore `fsGroup`). |
| Backups work but the dataset's `USED` stays ~0 while the pool's grows | The dataset is not mounted in the host namespace, so the API bind-mounted the parent dataset's directory. On Talos a `zfs create` from inside a pod mounts only in that pod's namespace (`mountPropagation: HostToContainer` is one-way), and writes through the mountpoint land on the **parent** dataset instead. Detect it with `df -h` from a fresh pod on the mount path: it must name `<pool>/naslos-buddy`, not the pool. Workarounds: reboot the node (the ZFS extension runs `zfs mount -a` at boot), or populate the dataset with `zfs receive` instead of writing through its mountpoint. |
| `the send stream ended early (N of at least M bytes)` | The node's `zfs send` died mid-stream, so nothing was published. Retry the same send: it continues the same chain and the buddy skips the chunks it already has. Note the dry-run estimate is an approximation (measured: +232 B on a 44 KB full send, +9.7 KB on a 53 MB full send, −120 KB on a 57 MB incremental), so the allowance is 1% and anything smaller is caught by `zfs receive` at restore time instead. |
| `quota exceeded: … bytes are already stored for this key` | The receiver's quota for this key is full. Prune, or raise `QuotaBytes` on the peer entry. |
| `the receiver already holds different bytes for chunk N` | The source changed while a push was interrupted. Start a new chain (drop `--resume`). |
| `cannot unwrap the data key` | The identity being used did not encrypt this backup. Restores need the sender's own identity file (private key **and** KEK). |
| `refusing to restore: manifest is not signed` | The manifest on the receiver was modified. Restore fails loudly; re-push the source. |

## 9. Status and what comes next

Implemented and tested in this change:

- `api/internal/buddy/` — identity/keys, request signing and replay protection,
  the chunked AES-256-GCM envelope, the receiver store, the peer registry, the
  receiver HTTP surface and the sender client (unit and end-to-end tests that drive
  a real receiver over HTTP).
- `api/cmd/buddyctl` — identity, enroll, push (dir/file/stream, resumable), status,
  backups, restore, prune.
- `api/cmd/buddy-receiver` + `api/Dockerfile.receiver` +
  `make buddy-receiver-image` — the standalone two-volume receiver.
- Receive side on a Naslos instance: `/api/buddy/v1/*`, `/api/buddy/status`,
  `/api/buddy/peers`, chart values, enrollment Secret, the dataset mount, and the
  UI proxy's body limit.
- **Instance-side sender and restore**: streaming `zfs send`/`zfs receive` seams in
  the agent (`/api/v1/zfs/send`, `/api/v1/zfs/receive`, `/api/v1/zfs/snapshots`),
  driven by `/api/buddy/send` and `/api/buddy/restore` (+ `verify`) on the API,
  with GUID-matched incrementals, a resumable interrupted send, an up-front
  sequence check for restores and a shortfall guard that keeps an incomplete stream
  from ever being published as the latest backup.

Next, in the order the plan calls for:

1. ~~**Backup page in the UI** — buddies, "Back up now", progress, free space and
   last-backup columns. The data plane and the API already provide all of it.~~
   **Done**: `/backups` shows the identity, schedules, live job progress,
   verify digests, a confirmation-gated restore form and the receiver status.
2. ~~**Scheduler + retention** — per-source schedule, `prune` after success, ntfy
   notification on failure.~~ **Done**: `hourly|daily|weekly` schedules with
   catch-up, `pruneKeep` on success and ntfy on success *and* failure.
3. ~~**Multi-buddy fan-out** — the same chain pushed to several receivers, with the
   last successful destination surfaced per buddy.~~
   **Done**: a schedule (or "Back up now") takes several receivers. Each destination
   gets its own job, chain and resume state, so a dead or busy buddy fails only
   itself; the entry stores the per-buddy outcome (`receiverResults`) and reports
   `ok` only when every destination stored the run. `pruneKeep` applies per
   destination, and each job sends its own notification.
4. ~~**Peer exposure** — decide between a dedicated listener and Traefik + Authelia
   for letting a peer reach `/api/buddy/v1/*` across networks (`SEC-9` keeps the
   agent's streaming endpoints in-cluster regardless).~~
   **Decided and verified: no dedicated listener.** The peer API rides the same
   Traefik ingress the UI already uses (the old UI NodePort was removed on
   2026-09-19). `/api/buddy/v1/*` stays public because a peer authenticates with
   its own Ed25519 key and cannot complete an interactive login; every
   owner-facing buddy route goes through the shared owner gate (SEC-10), so the
   same entry point serves both without weakening either. Live check with the
   gate armed: `/api/users` → `401` without the proxy secret, while `buddyctl
   enroll` + `push` through the ingress succeeded (enrolled, then `backed up
   exposure-test/data to chain 2b36eeb7…`). `SEC-9`'s constraint still holds: the
   agent's `/api/v1/zfs/*` streaming endpoints remain ClusterIP-only and never
   appear on the UI's nginx.



