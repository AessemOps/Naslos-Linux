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
BUDDY1
PUT
/api/buddy/v1/chunks/naslos-a/data?chain=1f3c&index=7
<sha256 hex of the request body>
1757843251
5c2nQw==
```

Because the method, the full URI (query string and mount prefix included), the body
digest, the timestamp and the nonce are all inside it, a captured request cannot be
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
| GET | `/chunks/{source}?chain=` | Chunk list: index + digest of each stored sealed chunk |
| GET | `/chunks/{source}?chain=&index=` | One sealed chunk (raw bytes) |
| PUT | `/chunks/{source}?chain=&index=` | Upload one sealed chunk (idempotent) |
| GET | `/manifest/{source}?chain=` | A manifest (`chain` omitted = current chain) |
| PUT | `/manifest/{source}` | Publish a signed manifest; refused unless every chunk it lists is present |
| POST | `/prune/{source}` | `{"keep": N}` — keep the newest N chains, delete the rest |

The index travels as a **query parameter** rather than a path segment because a
source can itself contain slashes (`naslos-a/tank/data`); guessing where the source
ends would be a correctness bug waiting to happen.

Owner-facing endpoints on a Naslos instance (not peer-facing):

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/api/buddy/status` | Receiver state for the UI: free space, peers, stored backups |
| GET/POST/DELETE | `/api/buddy/peers` | List / authorize / revoke peer keys |

`/api/buddy/peers` hands out storage access, so it requires the same
authenticated-session evidence as the terminal (`buddy.requireAuth`, default
`true`; it reads the proxy's identity header). The peer API is deliberately *not*
gated that way: a peer cannot complete an interactive login, which is the whole
reason it authenticates with its own key.

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
" NBC1 " magic(4) | plainLen(4, big-endian) | nonce(12) | AES-256-GCM ciphertext + tag
```

- **Key** — a fresh 256-bit data key (DEK) per chain segment.
- **Nonce** — `streamPrefix(8 random bytes) || chunkIndex(4, big-endian)`, unique
  per (key, index) by construction, which is the property GCM depends on.
- **AAD** — `NB1|<source>|<chain>|<index>|<plainLen>`, so chunks cannot be
  reordered, swapped between chains, moved between sources, or truncated
  undetected.
- **DEK wrapping** — the DEK is sealed with the owner's KEK and travels in the
  manifest (`AAD: NB1|dek|<source>|<chain>`), so a restore needs the owner's KEK
  and nothing from the receiver.

The manifest carries the chain's metadata, the chunk list (`index`, plain bytes,
sealed bytes, SHA-256 of the **plaintext**), the wrapped DEK, the sender's key id,
and an Ed25519 signature over all of it. The receiver can verify the signature and
the presence of every chunk; only the owner can check the plaintext digests — which
is exactly the intended asymmetry.

## 5. Operating it

### 5.1 Receiver: a Naslos instance

```bash
# 1. A dataset of its own for received chunks (the API is not a host writer, so
#    this is a deliberate operator step). On the VM layout that is:
zpool list                                   # e.g. pool "test"
zfs create test/naslos-buddy                 # -> /var/mnt/test/naslos-buddy

# 1b. Hand the dataset to the API's user. The API image is distroless and runs as
#     its unprivileged "nonroot" user (uid 65532); a dataset created by root is
#     mode 0755 root:root, so without this the first push fails with
#     "mkdir /var/lib/naslos/buddy/<key>: permission denied". fsGroup does NOT
#     help for a hostPath volume, so the ownership has to be set here:
chown 65532:65532 /var/mnt/test/naslos-buddy

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

Peers reach this API the same way the UI does — through the NodePort or ingress you
already expose, on the `/api/` path. **No new port and no Traefik requirement**;
nothing else about the deployment changes.

The nginx in front of the UI raises `client_max_body_size` to 8 MB for
`/api/buddy/` specifically, because its default of 1 MB would reject every 1 MiB
chunk with a 413 before it reached the API. That is the kind of thing that only
shows up on the first real push, so the location exists from the start.

### 5.2 Sender: `buddyctl`

```bash
# create (or show) this instance's identity — its public key is what a receiver authorizes
buddyctl identity --name naslos-a

# authorize that key on a receiver that has a token (the token is single use)
buddyctl enroll http://192.168.1.96:30080 --token <token> --sources naslos-a/

# a directory (archived as tar) — no ZFS needed on either side
buddyctl push http://192.168.1.96:30080 --source naslos-a/data --dir /var/mnt/test/data

# a ZFS stream, incrementally, resumable
zfs snapshot test/data@buddy-$(date +%F)
zfs send -w -i @yesterday test/data@buddy-$(date +%F) \
  | buddyctl push http://192.168.1.96:30080 --source naslos-a/data --kind zfs-send

# what the receiver knows: space left, what is stored, when the last backup was
buddyctl status http://192.168.1.96:30080
buddyctl backups http://192.168.1.96:30080

# verify without writing anything, then restore
buddyctl restore http://192.168.1.96:30080 --source naslos-a/data --verify-only
buddyctl restore http://192.168.1.96:30080 --source naslos-a/data --dir ./restored

# or straight back into ZFS
buddyctl restore http://192.168.1.96:30080 --source naslos-a/data | zfs recv test/data-restored

# retention, asked of the receiver
buddyctl prune http://192.168.1.96:30080 --source naslos-a/data --keep 7
```

`buddyctl` is built from the repo (`make buddyctl` → `bin/buddyctl`) and needs only
network access to the receiver: it is not a cluster component.

An interrupted push is continued, not restarted:

```bash
buddyctl push $RECEIVER --source naslos-a/data --dir /var/mnt/test/data   # dies at 80%
buddyctl push $RECEIVER --source naslos-a/data --dir /var/mnt/test/data --resume
```

### 5.3 Receiver: the standalone container

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
- **No scheduler yet.** Pushes are operator-driven (`buddyctl`) or pipeline-driven
  (`zfs send | buddyctl push`). Scheduling, and streaming `zfs send` from the
  instance itself, come next (§8).
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
| `mkdir /var/lib/naslos/buddy/SHA256_…: permission denied` | The receive dataset is not writable by the API's user. The API is distroless and runs as uid 65532: `chown 65532:65532 /var/mnt/<pool>/naslos-buddy` (hostPath volumes ignore `fsGroup`). |
| Backups work but the dataset's `USED` stays ~0 while the pool's grows | The dataset is not mounted in the host namespace, so the API bind-mounted the parent dataset's directory. On Talos a `zfs create` from inside a pod mounts only in that pod's namespace (`mountPropagation: HostToContainer` is one-way). Reboot the node, or mount it from a host-context process, then restart the API deployment. Detect it with `df -h` from a fresh pod on the mount path: it must name `<pool>/naslos-buddy`, not the pool. |
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

Next, in the order the plan calls for:

1. **Backup page in the UI** — buddies, "Back up now", progress, free space and
   last-backup columns. The data plane and the API already provide all of it.
2. **Instance-side ZFS streaming** — a streaming exec seam in the agent (its
   `hostExec` buffers output today, and the API's agent client has a 180 s timeout),
   so an instance can run `zfs send -w -i` itself instead of an operator piping into
   `buddyctl`.
3. **Scheduler + retention** — per-source schedule, `prune` after success, ntfy
   notification on failure.
4. **Multi-buddy fan-out** — the same chain pushed to several receivers, with the
   last successful destination surfaced per buddy.



