# Dataset encryption at rest (FR-ENC)

**Status:** design + safe validation slice only. No key is generated, stored,
accepted or loaded yet, no dataset is created encrypted, and there is no
unlock/lock or encrypted-send support. The normative requirements and their
`[OPEN]` markers are in [spec.md](spec.md#312-dataset-encryption-at-rest-fr-enc).

This document records the decision the parity backlog asked for (key custody for
an unattended Talos boot), the ZFS facts the implementation must respect, the
difference from Buddy Backup's zero-knowledge model, and the rule that existing
datasets cannot be converted in place.

## Why: what encryption at rest does and does not cover

ZFS native encryption protects the **data on the disks** and **raw send
streams**: someone who steals a disk, a pool member, or a raw backup cannot read
the blocks without the dataset key. It does **not** protect against:

- a compromised node, host root, `naslos-agent`, or `naslos-api` — while a
  dataset is unlocked the key is in kernel memory and the privileged agent can
  load any key it can read;
- anyone who can read the Kubernetes Secret that holds the key (or restore the
  control plane's etcd), because a Secret is a cluster object, not a hardware
  root of trust;
- a mounted and unlocked dataset — SMB/NFS/apps read plaintext normally;
- network interception — that is TLS/Authelia, not ZFS.

So this feature is **encryption at rest**, not end-to-end encryption. The
zero-knowledge backup path remains Buddy ([FR-BUD-02](spec.md#38-buddy-backup-fr-bud));
see [Buddy vs ZFS native encryption](#buddy-vs-zfs-native-encryption).

## ZFS native encryption: the facts the design must respect

From `zfsprops(7)`, `zfs-create(8)`, `zfs-load-key(8)` and `zfs-send(8)` on the
Talos ZFS build (OpenZFS 2.4.x):

| Fact | Consequence |
| --- | --- |
| `encryption` must be set **at dataset creation** and cannot be changed afterwards | No `zfs set encryption=…`; no in-place conversion (see [Migration](#migration-existing-datasets-cannot-be-converted-in-place)) |
| `encryption=on` selects `aes-256-gcm`; explicit `aes-128/192/256-{ccm,gcm}` are valid | The allow-list is explicit; `on` is normalised to `aes-256-gcm` |
| A create needs a `keyformat` (`raw`, `hex`, `passphrase`) | `raw` (32 random bytes) is the recommended keyfile format; `hex` is not offered |
| `keylocation` can be `prompt`, `file:///absolute/path`, or an HTTP(S) URL; it can be changed later with `zfs set`/`zfs change-key` | The request must never choose it: an arbitrary path or URL is a host-file/SSRF primitive. The agent owns the location |
| With `keylocation=prompt`, a non-TTY `zfs load-key` reads the key from **stdin** | A future unlock must pass the key on stdin; never in argv, env or `-o` |
| A passphrase is 8–512 bytes and runs through PBKDF2 (default 350 000 iterations) | Passphrases are expensive to brute-force, but still need a human after every boot |
| Raw keys must be 32 random bytes regardless of the cipher | Key generation is a CSPRNG operation, not a hash of a name |
| Child datasets inherit their encryption root and **share its key**; loading the encryption root loads the inheriting children | Lock/unlock is per encryption root, not per child; creating a child with its own `encryption=` starts a new root and key |
| Encrypted datasets may not use `copies=3` | The create validator rejects that combination |
| `zfs create -p` **succeeds without applying properties when the target already exists** | A create must verify that the exact dataset is encrypted and `keystatus=available`; exit status alone is not proof |
| `zfs send -w` sends raw ciphertext and works **without the key loaded**; a non-raw send of an encrypted dataset sends plaintext, and `-p`/`-R` refuse it without `-w` or `-U` | Raw send is the only safe replication path for an encrypted source; `-U` (no-preserve-encryption) must never be used for an untrusted target |
| A received raw stream keeps the **source key** and defaults `keylocation` to `prompt` | The destination needs the same key (or passphrase) to unlock; raw replication is same-key replication, not per-site re-encryption |

`zfs change-key` can rewrap the key or change `keyformat`/`keylocation`, so key
rotation is possible later without re-sending data. It cannot turn encryption on
or off.

## Boot and key custody on Talos

The `siderolabs/zfs` extension's `zfs-service` is the only boot integration:
it waits for `/dev/zfs` and runs `zpool import -fal`; on shutdown it runs
`zfs unmount -au` and `zpool export -a`. It does **not** pass `-l` and does not
run `zfs load-key -a`. Naslos does not fork the extension (FR-STO-04), so:

- an imported pool comes back, but every encrypted dataset whose key was not
  loaded is **locked** (`keystatus=unavailable`, `mounted=no`);
- a key file in ephemeral `/run` does not exist after a reboot, so even a boot
  service that ran `zfs load-key -a` would fail until the key is restored;
- the dataset must be unlocked before its mountpoint exists for Samba, NFS,
  apps, snapshots, replication or Buddy.

That makes the unlock model a product decision, not an implementation detail.

### Decision: keyfile in a Kubernetes Secret + explicit unlock

**Recommended and recorded for FR-ENC:** each encrypted dataset gets a
generated 32-byte raw key, stored in a `naslos-dataset-key-*` Secret in the
`naslos` namespace. After boot an administrator explicitly unlocks the dataset;
there is no automatic unlock by default. A per-dataset passphrase remains an
option later for data that must survive a Secret/etcd compromise, at the cost of
a human on every boot.

| | Keyfile in a K8s Secret + explicit unlock (chosen) | Passphrase only |
| --- | --- | --- |
| Where the key lives | 32-byte raw key in a Secret; fetched on unlock and written to an agent-owned 0600 file on host tmpfs only for `zfs load-key` | Operator's memory only; never persisted |
| Create | API generates the key, writes the Secret, then sends the key to the agent over the authenticated channel; agent creates with `keyformat=raw`, `keylocation=file:///run/naslos-keys/<id>` | Operator supplies a passphrase; `keyformat=passphrase`, `keylocation=prompt` |
| Boot on Talos | Locked after boot; one admin unlock (API fetches the Secret, agent loads the key) | Locked after boot; an operator must re-enter the passphrase |
| Unattended uptime | Good after the unlock; a future opt-in auto-unlock is possible but weakens the model | None: every reboot is an outage until a human acts |
| Scheduled snapshots / replication / Buddy | Work after unlock | Fail while locked |
| Threat model vs a Secret reader / etcd backup | Weaker: the reader can unlock | Stronger: no stored key; but the passphrase transits API/agent memory |
| Recovery | Back up the Secret; losing it loses the data | Losing the passphrase loses the data |
| Operational fit for a headless NAS | Good | Poor for shares that must come back automatically |

The chosen model is the same trust level as the secrets Naslos already keeps in
Kubernetes (Authelia, Buddy's KEK, DDNS credentials). It is a deliberate
trade-off: **explicit unlock is an authorization gate and a boot-order guard,
not protection from a cluster administrator.** The Secret must be included in
the operator's config/secret backup, or the data is unrecoverable.

### Proposed shape (all `[OPEN]`)

- **Create:** API validates the shape → API generates 32 random bytes with
  `getrandom` → API writes Secret `naslos-dataset-key-<id>` (labels: dataset,
  pool) → API calls the agent with the key in the JSON body over the existing
  authenticated TLS channel → agent writes the key to
  `/host/run/naslos-keys/<id>` (0600), runs
  `zfs create -o encryption=… -o keyformat=raw -o keylocation=file:///run/naslos-keys/<id>`,
  then verifies `encryption` and `keystatus=available` on the exact dataset.
  On failure the partial dataset is destroyed and the Secret deleted.
- **Unlock:** admin-only `POST /api/datasets/.../unlock`; API fetches the Secret
  and sends the key to the agent; the agent writes the 0600 tmpfs file,
  `zfs load-key -L file:///run/naslos-keys/<id> <dataset>`, and never logs the
  body or the key. Repeated unlock is idempotent.
- **Lock:** admin-only; refused while the dataset is mounted or used by a
  share/app; `zfs unload-key <dataset>` and remove the tmpfs key. The data stays
  on disk, unreadable until the next unlock.
- **Status:** `GET /api/datasets` gains `encryption`, `keystatus` and
  `encryptionroot`; the UI marks locked datasets and disables share/app actions.
- **Key material never** appears in argv, environment variables, `-o`
  properties, logs, error messages, API responses, or a persistent host file.
  The only transient copy is the 0600 tmpfs file used by `zfs load-key`.

## Encrypted send and receive

- **Raw send (`zfs send -w`)** is the required path for an encrypted source: it
  sends the blocks exactly as stored, works even when the key is not loaded, and
  the receiver stores ciphertext it cannot read.
- **Never `-U`/`--no-preserve-encryption`** to a target that is not fully
  trusted: it writes plaintext into the stream. A property-bearing (`-p`/`-R`)
  send of an encrypted dataset must be refused rather than silently downgraded.
- **Same-key receive:** a raw stream keeps the sender's encryption key, so the
  receiving dataset can only be unlocked with that same key/passphrase. Raw
  replication is therefore not per-site re-encryption; if independent keys are
  required, the destination must be encrypted separately and the source must be
  decrypted for the transfer (which requires the source key and exposes
  plaintext to the replicating host).
- **Incremental raw sends** require the destination to hold the raw-received
  chain; the existing replication GUID bookkeeping is unchanged, but the job
  must record that the chain is encrypted and must never mix raw and non-raw
  streams.
- **Buddy:** see the next section. The recommendation is that Buddy sends raw
  ciphertext for an encrypted dataset (`zfs send -w`) so the API never handles
  plaintext, then keeps its own AES-256-GCM envelope. That is a change to the
  Buddy send path and is `[OPEN]`.

## Buddy vs ZFS native encryption

| | ZFS native encryption | Buddy Backup (FR-BUD-02) |
| --- | --- | --- |
| Protects against | stolen disks/media, offline pool members, raw backups | a compromised or curious **backup receiver**, and data in transit to it |
| Key holder | the node (Secret + loaded kernel key) | the owner's KEK, never the receiver |
| Receiver can read? | yes, once it has the same key (raw receive keeps the source key) | no: it stores ciphertext and a wrapped per-chain DEK |
| Granularity | per encryption root (dataset tree) | per chain / per source, client-side AES-256-GCM |
| Boot/automation | locked until unlocked | works without mounting the source dataset as a share; needs the source readable |
| Replaces the other? | no | no |

**FR-BUD-02 is unchanged and is not weakened by this feature.** Buddy must keep
encrypting client-side before data leaves the sender; a ZFS-encrypted dataset
must not be treated as "already encrypted" for backup purposes, and the Buddy
receiver must still be unable to derive the KEK. The two layers are
complementary: ZFS protects the local disks, Buddy protects the off-site copy.

## Migration: existing datasets cannot be converted in place

`encryption` is create-only (`zfs set encryption=…` is not valid, and
`zfs change-key` only rewraps an existing key). The migration rule is therefore:

1. **Stop** every share, app mount, replication job and snapshot job that uses
   the dataset; record the current mountpoint, quota, compression and ACL
   properties.
2. **Snapshot** the source (`pool/data@pre-encryption`) and keep it for
   rollback.
3. **Create the encrypted target** (new name, own key, same properties and
   mountpoint plan) and stream the data into it: a full `zfs send` of the
   unencrypted source can be received with `-o encryption=… -o keyformat=raw
   -o keylocation=…`; incrementals follow. Receive into a *new* dataset, never
   over the old one.
4. **Verify** `keystatus=available`, the snapshot list, a sample of file
   contents and checksums (or `zfs diff`), before repointing anything.
5. **Repoint** shares/apps to the new dataset's mountpoint (share paths are
   path-based; renaming a dataset does not change Samba/NFS paths by itself).
6. **Keep the old dataset read-only** for a rollback window, then destroy it and
   its snapshots. Destroying the old copy does not securely erase blocks, and
   destroying the key Secret is irreversible (crypto-shredding).

There is **no one-click "encrypt this dataset"** and no automatic migration in
v1. A migration must be rehearsed on a scratch dataset first; it is a
data-moving operation, not a property change.

## Current implementation (this slice)

Only the pure planner is implemented, in both layers:

- `rust/agent/src/zfs/encryption.rs` and `rust/api/src/dataset_encryption.rs`
  parse `encryption`, `keyformat` and `keylocation` against the allow-list and
  share the fixture `rust/testdata/dataset_encryption.txt`.
- A dataset-create request that asks for encryption is **refused before any
  `zfs` command runs** with the stable message
  `dataset encryption is not implemented yet: key custody, unlock/lock and encrypted send are [OPEN] ...`.
- `keylocation` is never accepted from a request; `copies=3` with encryption,
  unknown algorithms, unknown key formats and missing `keyformat` are rejected.
- No key material is accepted, generated, stored, logged or returned. A
  `key`/`passphrase` field is not an allow-listed dataset option and is rejected
  by the existing option allow-list.

This is intentional: creating an encrypted dataset before custody/unlock exist
would produce a dataset that is locked after the next reboot with no supported
way to unlock it.

## Open work for full implementation

| Area | Requirement | Remaining work |
| --- | --- | --- |
| Key custody | FR-ENC-03 | Secret naming/RBAC, CSPRNG generation, agent create/lookup contract, zeroization, no-log tests |
| Unlock/lock | FR-ENC-04 | API routes, `zfs load-key`/`unload-key`, stdin/file handling, idempotence, mount ordering |
| Passphrase option | FR-ENC-05 | stdin transport through the agent runner, UI one-time entry, no persistence tests |
| Status/UI | FR-ENC-04/09 | `encryption`/`keystatus`/`encryptionroot` in the dataset contract and UI, locked-state actions |
| Create atomicity | FR-ENC-06 | Secret-first ordering, rollback, exact-dataset verification instead of trusting `zfs create -p` exit status |
| Encrypted send | FR-ENC-07 | raw-send job flag, `-U` prohibition, same-key receive UX, mixed-chain refusal |
| Buddy raw send | FR-ENC-08 | decide and implement `-w` for encrypted sources without changing the receiver's zero-knowledge guarantee |
| Shares/apps | FR-ENC-09 | refuse locked paths in share/app preflight and reconciliation |
| Key rotation/backup | FR-ENC-10 | `zfs change-key` flow and the config/secret export story |
| Migration tooling | FR-ENC-01 | optional guided create+send/receive migration; never an in-place conversion |
