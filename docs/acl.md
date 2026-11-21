# Access control (ACL) — decision spike

**Status:** decision record for P0-4 of the TrueNAS parity backlog.
**Scope:** dataset and share permissions on Naslos.
**Implementation today:** a pure preset and lockout-guard module
(`rust/api/src/shares/acl.rs`). The editor, inheritance, effective-access
preview, Samba/Windows ACL mapping, NFSv4 datasets and migration are **[OPEN]**.

This document answers one question before any editor is built:

> Do we add a POSIX ACL editor on today's `acltype=posixacl` datasets, or move
> SMB datasets to `acltype=nfsv4` and map Windows ACLs through Samba?

The recommendation is at the end; the short version is **POSIX presets first,
NFSv4 deferred to a separate decision with its own migration tooling**.

## 1. Current state (verified in this tree)

| Layer | What exists today | Where |
| --- | --- | --- |
| Pool defaults | `xattr=sa`, `compression=zstd`, `acltype=posixacl`, `atime=off`, … | `rust/agent/src/zfs/operations.rs` (`default_options`) |
| Dataset creation | A small safe property allow-list: `compression`, `quota`, `recordsize`, `atime`, `copies`, `readonly`. ACL properties are **not** settable through the API | `rust/agent/src/zfs/dataset.rs` |
| Dataset ownership | Shared data model is owner `10001` and group `10000` (`admin:naslos_users`); app containers run with the same gid. Directory mode/`setgid` is **not** provisioned by dataset creation today | `docs/app-catalog.md`, `rust/agent/src/zfs/dataset.rs` |
| API identity | Non-root `65532`, supplementary gid `10000`; mounts datasets read-only (list/stat only) | `charts/naslos/templates/api-deployment.yaml` |
| Agent identity | Privileged root DaemonSet; the only component that writes host configuration | `rust/agent`, `docs/shares.md` |
| SMB | Samba, `security = user`, `map to guest = Never`; sessions use the LDAP user's POSIX identity mirrored through `extrausers`; no `force user`; `create mask = 0660`, `directory mask = 0770`; `vfs objects = fruit streams_xattr` | `rust/api/src/shares/render.rs`, `samba/image/` |
| NFS | NFS-Ganesha (userspace), NFSv4/TCP, `SecType = sys` (AUTH_SYS), `Root_Squash` unless a share opts out; no Kerberos | `rust/api/src/shares/render.rs`, `docs/shares.md` |
| Share access | Per-share `validUsers`, `validGroups` (`@group`), `allowedHosts`, `readOnly`, `timeMachine` | `docs/shares.md` |
| ACL editor | **None.** No API route, no UI page, no `setfacl`/`getfacl` call | this repo |

**FR-STO-01 remains pinned:** every pool is created with
`acltype=posixacl`, and changing that default is explicitly out of scope for
this slice. A move to NFSv4 ACLs would need a new requirement and a migration
plan, not a silent default flip.

## 2. The access stack

ACL evaluation is only one of several gates. A useful editor must say which
layer it controls; a preview that merges all of them without saying so is
misleading.

```
client request
  │
  ├─ protocol gate      Samba session / NFS AUTH_SYS uid+gid
  ├─ share gate         validUsers / validGroups / allowedHosts / readOnly
  ├─ dataset ACL        POSIX mode bits + ACL entries (or NFSv4 ACEs)
  └─ filesystem state   ZFS acltype/aclmode/aclinherit, setgid, umask/masks
```

The guarded identity in the shipped module is the **platform service
identity** (API `65532` with gid `10000`, app workloads `10001:10000`), not
root and not an interactive SMB user. Root bypasses DAC checks, so it is not a
meaningful subject for a lockout guard.

## 3. Option A — POSIX ACL editor on today's datasets

### 3.1 What the editor would change

A preset maps to a base POSIX ACL:

| Preset | owner | group | other | mode | Typical use |
| --- | --- | --- | --- | --- | --- |
| `private` | `rwx` | `---` | `---` | `0700` | Personal dataset |
| `group-read` | `rwx` | `r-x` | `---` | `0750` | Shared read-only |
| `group-write` | `rwx` | `rwx` | `---` | `0770` | Current Naslos default |
| `public-read` | `rwx` | `r-x` | `r-x` | `0755` | Media/library export |

That is the allow-list implemented in `shares::acl`. Named user/group entries
and arbitrary ACL text are deliberately excluded from the first slice: a
free-form editor is where ACL lockouts, typos and malformed entries enter.

### 3.2 Inheritance

POSIX has **default ACLs** on directories. A default ACL is copied into the
access ACL of a new child, so inheritance is per-directory and aimed downward.
It has no propagation flags, no "inherit-only" entries, no deny ACEs and no
per-object inheritance toggles. If the editor needs "apply to existing children
too", that is a separate, recursive operation with its own confirmation.

For a first editor this is acceptable: set the default ACL on a dataset root or
share folder, and new content inherits. It is not Microsoft-equivalent
inheritance.

### 3.3 Samba implications

Samba can use POSIX ACLs directly, but the current renderer does not enable or
tune Samba's ACL layer:

- `vfs objects = fruit streams_xattr` is global; `acl_xattr` and
  `nfs4acl_xattr` are absent.
- `create mask`/`directory mask` (`0660`/`0770`) can strip bits that an ACL
  would otherwise grant. Masks, `inherit acls`, `nt acl support` and the ACL
  backend must be decided together, not independently.
- Without `acl_xattr`, Samba approximates NT ACLs with POSIX ACLs. Windows
  clients will see a reduced model: no deny ACEs, no fine-grained rights, and
  inheritance that looks different from Windows.
- With `acl_xattr`, Samba can store an opaque `security.NTACL` xattr. That
  preserves Windows intent but creates a split brain unless it mirrors system
  ACLs: NFS and non-SMB readers only see POSIX mode bits and the POSIX ACL.
  `acl_xattr:ignore system acls = yes` makes the split explicit; it is not a
  free win.
- SMB sessions run as the authenticated user's POSIX identity (mirrored through
  `extrausers`). An ACL change that removes the owning group's access can lock
  out the very account that mounts the share, even though the Samba account and
  password still work.

### 3.4 NFS implications

Ganesha serves AUTH_SYS numeric uid/gid. The export can enforce POSIX mode bits
and, where the FSAL supports them, POSIX ACLs; NFSv4 ACL support in Ganesha's
VFS FSAL is unproven on this stack and must not be assumed. There is no
Kerberos identity, no idmapping and no `sec=krb5`. Clients that mount the
export need matching numeric ownership or a group in common. Root squash means
a client root cannot repair permissions unless the share explicitly opts out.

### 3.5 Effective access

For a POSIX preset the base calculation is deterministic: matching owner uid
wins, then any matching gid, otherwise the other class; the ACL mask caps the
group class. The shipped module implements exactly that. It still does **not**
know:

- the share's `validUsers`/`validGroups`/`allowedHosts` gates;
- protocol differences (SMB session uid vs NFS AUTH_SYS uid);
- supplementary groups from LDAP at the moment of access;
- Samba's `create mask`/`directory mask` or `acl_xattr` behavior;
- default ACLs inherited from a parent folder.

An effective-access preview must therefore be labelled by layer and must not
claim to be a live probe. That work is **[OPEN]**.

### 3.6 Migration fallout

None for the default path: the pool, datasets and existing ACLs stay as they
are. The only change is that an editor can write presets onto existing
`posixacl` datasets once the write path exists. That is the main reason this
option is attractive.

### 3.7 Trade-offs

**Pros**

- No dataset conversion and no mixed `acltype` tree.
- Works with both SMB and NFS through the existing POSIX semantics.
- Fits the current `10001:10000` ownership model and app containers.
- Small, auditable preset surface; easy lockout guard.
- Keeps FR-STO-01 and every existing snapshot, clone and send/recv stream
  meaningful.

**Cons**

- No deny ACEs, fine-grained Windows rights or real Windows inheritance.
- A Windows admin who expects to set an ACL in Explorer will see it
  approximated.
- Default ACLs only inherit downward; recursive changes need explicit tools.
- Samba's ACL backend and masks still need tuning before the model is honest.

## 4. Option B — `acltype=nfsv4` datasets + Windows ACL mapping

### 4.1 What would change

- Pool/dataset default becomes `acltype=nfsv4` (a change to FR-STO-01).
- `aclmode` and `aclinherit` become part of the dataset contract
  (`passthrough` is the likely choice for Windows fidelity; the current code
  sets neither and relies on ZFS defaults).
- Samba needs a VFS ACL backend that can store and evaluate NFSv4 ACLs, plus
  idmapping for Windows SIDs. `nfs4acl_xattr` is the candidate; it must be
  proven against Linux ZFS's `system.nfs4_acl` representation and against the
  Debian Samba build in the image.
- The API's safe dataset property allow-list would need to expose ACL
  properties, with their own confirmation and migration warnings.

### 4.2 Samba implications

NFSv4 ACLs can represent allow/deny ACEs, inheritance flags and Windows-style
rights, which is the point of the option. The risks are:

- **Backend split.** If Samba stores Windows ACLs in an opaque xattr while NFS
  sees mode bits, SMB and NFS enforce different permissions on the same files.
  A NAS that serves both protocols must not hide that.
- **Idmapping.** Windows SIDs must map to POSIX ids (or be stored numerically).
  The current identity model is LDAP uid/gid plus `extrausers`; there is no
  AD/SID mapping yet, and P0-5 (directory + Kerberos) is a separate backlog
  item.
- **Masks and inheritance.** `create mask`, `directory mask`, `inherit acls`,
  `map acl inherit` and `aclinherit` interact. Setting ACLs without agreeing on
  these produces surprising results.
- **Client behavior.** Explorer, Finder and Time Machine all create and rewrite
  ACLs differently; macOS `fruit` and `streams_xattr` must be tested with the
  chosen backend.

### 4.3 NFS implications

NFSv4 defines an ACL attribute, but whether Ganesha's VFS FSAL exposes and
enforces it — including deny ACEs, inheritance and `supported_attrs` — is
unproven on this stack. Until it is verified, an NFSv4 dataset could give SMB
rich ACLs and NFS a different, mode-bit-only view. That is worse than an honest
POSIX model.

### 4.4 Migration fallout

This is the expensive part, and it is why the option should not be smuggled into
a default change:

- **No free in-place conversion.** `acltype` is a dataset property. Changing it
  does not translate existing POSIX ACLs into NFSv4 ACEs; files that had ACLs
  can silently change meaning, and chmod/aclinherit behavior changes with them.
  Any in-place flip must be treated as data-affecting and tested, not assumed.
- **Mixed tree.** Child datasets inherit `acltype` at creation. A pool or parent
  switch makes new children NFSv4 while existing children remain POSIX. The UI
  and API would have to expose and validate per-dataset ACL type forever.
- **Send/receive.** `zfs send | zfs receive` preserves ACL data in the stream,
  but the receiving dataset's `acltype`/`aclinherit` decides how it is
  interpreted. A rollback or a restore to a POSIX dataset is not lossless
  without an explicit conversion step and a validation pass.
- **Snapshots and clones.** Existing snapshots carry POSIX ACLs; clones inherit
  the origin's type. A migration must account for both, not just the live tree.
- **Applications.** App datasets are documented as `10001:10000` with group
  access; dataset creation does not apply a mode or setgid bit today. An app
  that runs `chmod` or `chown` can rewrite NFSv4 ACEs (or be blocked), depending
  on `aclmode`. Every chart would need a compatibility check.
- **Backups.** Native `zfs send` preserves ACLs. The Buddy `tar` directory path
  does not: it writes directory entries with mode `0755` and does not carry
  POSIX ACLs or xattrs (`rust/api/src/bin/buddyctl.rs`). A restore from that
  path therefore does not preserve the permission model. Native `zfs send`
  replication (P0-2 in the backlog) is the better long-term carrier; neither is
  a reason to convert datasets before the backup path is proven.
- **Recovery.** A bad ACL change can lock out the service account or an admin
  client. Recovery needs console/agent access, an ownership snapshot and a
  rehearsed rollback. The current UI has no permission editor, so there is no
  established recovery path to build on.

### 4.5 What a real NFSv4 spike must prove

1. Samba's `nfs4acl_xattr` (or the chosen backend) works with Linux ZFS
   `acltype=nfsv4` on Talos, including inheritance and deny ACEs.
2. Ganesha's VFS FSAL exposes and enforces NFSv4 ACLs for NFSv4 clients, or the
   product explicitly accepts an SMB-only ACL model.
3. `aclmode`/`aclinherit` behavior with app `chmod`/`chown`, Time Machine and
   `fruit` is understood and tested.
4. A `zfs send` to a new NFSv4 dataset, plus a rollback, preserves and
   reinterprets ACLs as documented.
5. Ownership and idmapping for Windows SIDs are defined (depends on P0-5).
6. A rehearsed lockout and recovery drill exists for the service account and an
   admin client.

Until all six have evidence, Option B is a research project, not an editor
feature.

## 5. Comparison

| Criterion | POSIX (A) | NFSv4 + Samba (B) |
| --- | --- | --- |
| Works on today's datasets | Yes | No — conversion required |
| FR-STO-01 unchanged | Yes | No |
| SMB Windows ACL fidelity | Approximate | Potentially exact |
| NFS semantics | POSIX, already understood | Depends on Ganesha support |
| Inheritance | Default ACLs, downward only | Rich, if backend is proven |
| Deny ACEs | No | Yes |
| Migration risk | None | Dataset-level, mixed-tree, backup-sensitive |
| App compatibility | Existing `10001:10000` ownership model | Needs per-chart validation |
| Dependencies | None | Samba backend, idmapping, Ganesha support, P0-5 |
| Time to first useful editor | Short | Long |

## 6. Recommendation

**Adopt Option A now and keep FR-STO-01 pinned.**

1. **Phase 1 (this slice):** fixed POSIX presets, typed entries and the
   service-account lockout guard in `shares::acl`. No UI, no writes, no ZFS
   property changes.
2. **Phase 2:** read-only inspection (owner, group, mode, base ACL, inferred
   preset) and an editor for the four presets. Every write goes through the
   privileged agent after validation, with an audit record and a dry-run.
3. **Phase 3:** default ACLs for inheritance on directories, with an explicit
   recursive-apply confirmation.
4. **Phase 4:** effective-access preview that names each gate (dataset ACL,
   share gate, protocol identity) and says what it does not know.
5. **Later, separately:** revisit NFSv4 with the spike checklist above, the AD/
   Kerberos work (P0-5) and a migration plan. A future `acltype=nfsv4` default
   requires a new FR and an explicit migration, never a silent flip.

This gives SMB/NFS users a safe, understandable permission editor without
turning every existing dataset into a migration project. It also leaves the
door open for Windows-fidelity ACLs once the identity and backup layers can
carry them.

## 7. What shipped in this slice

- `rust/api/src/shares/acl.rs`:
  - the four-preset allow-list and stable wire names;
  - POSIX mode bits and typed base entries (`u::`, `g::`, `o::`, `m::`);
  - owner/group/other effective-permission resolution, including supplementary
    groups;
  - `validate_for_service_account`, which refuses a preset that would remove
    the required access (`r-x` for read-only, `rwx` for read-write);
  - unit tests for modes, entries, name round-trips, no world-write, class
    resolution and lockout refusal.
- `docs/spec.md` — new `FR-ACL` group with inline test references.
- This decision record.

Nothing in this slice reads or writes a live ACL, constructs a `setfacl`
command, changes a dataset property or alters FR-STO-01.

## 8. Remaining [OPEN] work

| Requirement | Remaining work |
| --- | --- |
| FR-ACL-03 | Editor UI/API: owner, group, preset, dataset/folder scope |
| FR-ACL-04 | Default ACLs and inheritance, including recursive apply |
| FR-ACL-05 | Effective-access preview across dataset ACL, share gate and protocol identity |
| FR-ACL-06 | Samba ACL backend, masks and Windows ACL mapping |
| FR-ACL-07 | NFSv4 dataset option, Ganesha ACL support and the six-point spike above |
| FR-ACL-08 | Migration/conversion tooling, mixed-tree handling and rollback |
| FR-ACL-09 | Live write path through the agent, audit, dry-run and lockout recovery |

## 9. References

- `docs/spec.md` — FR-STO-01, FR-SHR-01…12, FR-ACL-01…09.
- `docs/shares.md` — share model, Samba/NFS rendering, identity mirror.
- `docs/storage-zfs.md` — pool creation and dataset defaults.
- `docs/app-catalog.md` — dataset ownership and app gid expectations.
- `rust/api/src/shares/acl.rs` — the shipped preset module.
