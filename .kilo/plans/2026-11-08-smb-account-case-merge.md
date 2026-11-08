# SMB account mirror: canonical uids and legacy duplicate cleanup

## Problem (live, 2026-11-08)

`elisa` could not authenticate to SMB while her LDAP/API password was valid.

Evidence from the node:

- LDAP has exactly one account: `uid=elisa`. The API state
  (`/var/lib/naslos/smbusers.json`) and the rendered node file
  (`/etc/naslos/shares/smbusers`) had **two** entries for the same POSIX user:
  `Elisa` (uid 10003, hash from 2026-11-06) and `elisa` (uid 10003, hash from
  2026-11-08).
- The API normalized the uid inside `create_person`/`set_password` but recorded
  the SMB mirror under the raw uid from the request (`Elisa` at creation). Later
  password changes use the canonical lowercase uid from the user list, so both
  keys accumulated.
- `pdbedit -i smbpasswd:<file>` processes the file top-down; the first
  `Elisa`/uid 10003 record creates the passdb account, and the later `elisa`
  record with the same uid is skipped. The passdb therefore kept the stale
  2026-11-06 hash. Importing only the lowercase line updated it, proving the
  duplicate uid is the blocker.
- The extrausers group file also rendered `naslos_users` twice
  (`10000` from the users' primary gid and `23170` from the LDAP group), because
  rendering keyed groups by gid instead of by name.

## Fix

1. `shares::SambaUserStore` canonicalizes every account key
   (`identity::normalize_uid`: trim + lowercase) on load and on
   `get`/`upsert`/`set_enabled`/`remove`.
2. `load` merges legacy case-variant duplicates, keeping the entry with the
   newest `passwordSetAt`, and persists the compacted JSON.
3. `render_group` keys groups by name so the LDAP `naslos_users` group merges
   into the primary-gid entry (one line per group name).
4. The API re-applies the share config once at startup (with a short retry), so
   a migrated account file reaches the node without waiting for the next user
   or share change.
5. Docs (`docs/shares.md`) record the case rule and the duplicate-uid pitfall.

## Tests

- Load compacts `Elisa`/`elisa` (same uid) to the lowercase, newest hash.
- Mutators work with mixed-case uids.
- `render_group` emits one `naslos_users` line when an LDAP group has the same
  name as the primary group.

## Found while drilling (not fixed here)

Deleting a user removes it from the rendered `smbusers` file and the NSS files,
but `pdbedit -i` never prunes an existing passdb record. The record is harmless
(its uid no longer resolves, so login is denied) but it lingers; the live drill
removed the test record with `pdbedit -x -u <uid>`. A future change can carry an
explicit prune list instead of implicit deletion from the file.

## Live remediation (already required for the reported node)

1. Delete the stale uppercase entries from the API state JSON and restart the
   API so the old image reloads only the canonical entries.
2. `POST /api/shares/apply` to push the clean `smbusers`/extrausers files.
3. Verify the passdb hash and the extrausers files for `elisa`.
