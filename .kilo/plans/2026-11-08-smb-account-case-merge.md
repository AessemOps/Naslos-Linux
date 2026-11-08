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
4. The API re-applies the share config once at startup (with a short retry) so
   a migrated account file reaches the node without waiting for the next user
   or share change. The apply is gated: it is skipped when the persisted shares
   or account mirror did not load (a missing/corrupt file must not overwrite a
   working node with "empty"), and it waits for a successful LDAP group read
   (a degraded group file would drop `naslos_admins`/`apps_user`).
5. A merged duplicate keeps the newest password but disablement wins
   (`enabled = existing.enabled && new.enabled`), so migration cannot resurrect
   an account an admin disabled.
6. The samba entrypoint prunes passdb accounts that are no longer in the
   rendered file, so deleting a user removes the stale passdb record instead of
   leaving an unresolvable one behind. Pruning only runs when the account file
   is non-empty; the agent writes it atomically (temp + rename), so a partial
   list is never observed.
7. Docs (`docs/shares.md`) record the case rule, the duplicate-uid pitfall and
   the prune behavior.

## Tests

- Load compacts `Elisa`/`elisa` (same uid) to the lowercase, newest hash.
- Mutators work with mixed-case uids.
- `render_group` emits one `naslos_users` line when an LDAP group has the same
  name as the primary group.

## Deletion prune (found while drilling, fixed here)

Deleting a user removed it from the rendered `smbusers` file and the NSS files,
but `pdbedit -i` never prunes an existing passdb record. The record was harmless
(its uid no longer resolved, so login was denied) but it lingered. The samba
entrypoint now reconciles the passdb to the rendered file after each import:
`pdbedit -x -u <name>` removes accounts whose canonical name is absent. A
missing/empty file skips the prune, and the agent's atomic write means the
rendered list is never partial. Drill: `smbprune` was created, logged in, deleted
through the API, and `pdbedit -x` removed the stale record as the entrypoint
would.

## Live remediation (already required for the reported node)

1. Delete the stale uppercase entries from the API state JSON and restart the
   API so the old image reloads only the canonical entries.
2. `POST /api/shares/apply` to push the clean `smbusers`/extrausers files.
3. Verify the passdb hash and the extrausers files for `elisa`.
