# External Directory (FR-EXT)

Status: **phase 1 — configuration only.** The internal OpenLDAP directory is
still the identity source of record and the only path used for login, password
changes and the Samba account mirror. At this stage the external directory is
validated at API startup and stored as optional configuration; no request-time
code reads it, no provider is contacted, and no authentication path changes.

Normative requirements live in [`spec.md`](spec.md) §3.2.1 (`FR-EXT`);
`FR-IDN-01…16` continue to apply unchanged.

## Why an external source

The internal directory is deliberately small and appliance-managed: users are
created in the Naslos UI, uidNumber allocation starts at 10001, and every
password change updates both LDAP and the SMB NT-hash mirror. Sites that
already run a directory want those identities to be usable without creating a
second copy by hand.

The planned model is **coexistence**, not replacement:

| Source | Role |
| --- | --- |
| Internal OpenLDAP (`dc=naslos,dc=local`) | Default source of record; writable through the UI/API; drives Authelia and the Samba mirror |
| External LDAP | Optional read-only identity source, added alongside the internal one in a later phase |

Phase 1 establishes the configuration contract so later phases can add the
read-only merge without changing chart values or environment names again.

## Configuration

The API reads these environment variables at startup. The Helm chart sets them
from `api.externalDirectory.*`; do not edit the Deployment directly.

| Chart value | Environment variable | Default | Notes |
| --- | --- | --- | --- |
| `enabled` | `EXTERNAL_DIRECTORY_ENABLED` | `false` | `true`/`false` (case-insensitive) |
| `provider` | `EXTERNAL_DIRECTORY_PROVIDER` | `ldap` | Only `ldap` in phase 1; `ad` is rejected |
| `host` | `EXTERNAL_DIRECTORY_HOST` | — | Hostname or IPv4 address, no scheme/port/path |
| `port` | `EXTERNAL_DIRECTORY_PORT` | `636` with TLS, `389` without | Non-zero |
| `useTLS` | `EXTERNAL_DIRECTORY_USE_TLS` | `true` | Plaintext is refused off loopback |
| `baseDN` | `EXTERNAL_DIRECTORY_BASE_DN` | — | Required; must contain `=` |
| `bindDN` | `EXTERNAL_DIRECTORY_BIND_DN` | — | Required service-account DN |
| `userSearchBase` | `EXTERNAL_DIRECTORY_USER_SEARCH_BASE` | `baseDN` | Must equal or sit under `baseDN` |
| `groupSearchBase` | `EXTERNAL_DIRECTORY_GROUP_SEARCH_BASE` | `baseDN` | Must equal or sit under `baseDN` |
| `uidAttribute` | `EXTERNAL_DIRECTORY_UID_ATTRIBUTE` | `uid` | LDAP attribute name |
| `uidNumberAttribute` | `EXTERNAL_DIRECTORY_UID_NUMBER_ATTRIBUTE` | `uidNumber` | POSIX uid mapping |
| `gidNumberAttribute` | `EXTERNAL_DIRECTORY_GID_NUMBER_ATTRIBUTE` | `gidNumber` | POSIX gid mapping |
| `groupMemberAttribute` | `EXTERNAL_DIRECTORY_GROUP_MEMBER_ATTRIBUTE` | `member` | Group membership |
| `bindPasswordSecret.name` / `.key` | `EXTERNAL_DIRECTORY_BIND_PASS_FILE` | — | Secret reference; the password is a mounted file |
| `caSecret.name` / `.key` | `EXTERNAL_DIRECTORY_CA_CERT` | — | Required with TLS; mounted CA bundle |

The bind-password and CA Secrets are mounted read-only at
`/var/run/secrets/external-directory/bind-password` and
`/var/run/secrets/external-directory-ca/ca.crt`. The Secret volumes are
optional so a half-created Secret cannot take the API down: validation fails,
the external source stays disabled, and the internal directory keeps serving.

### Secret shape

Create the Secrets before enabling the block; do not put credentials in
`values.yaml`.

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: external-directory-bind
  namespace: naslos
type: Opaque
stringData:
  bind-password: "replace-me"
---
apiVersion: v1
kind: Secret
metadata:
  name: external-directory-ca
  namespace: naslos
type: Opaque
stringData:
  ca.crt: |
    -----BEGIN CERTIFICATE-----
    ...
    -----END CERTIFICATE-----
```

Then supply only the references:

```yaml
api:
  externalDirectory:
    enabled: true
    provider: ldap
    host: ldap.example.com
    baseDN: dc=example,dc=com
    bindDN: cn=naslos-svc,ou=services,dc=example,dc=com
    userSearchBase: ou=people,dc=example,dc=com
    groupSearchBase: ou=groups,dc=example,dc=com
    bindPasswordSecret:
      name: external-directory-bind
      key: bind-password
    caSecret:
      name: external-directory-ca
      key: ca.crt
```

### TLS and CA

* LDAPS is mandatory for a non-loopback host. There is no "skip verification"
  option: a TLS configuration without `caSecret` fails validation rather than
  trusting an unverified certificate. (Phase 1 never dials the provider; the
  live read path in phase 2 will use this validated TLS configuration.)
* The CA bundle may contain one or more PEM certificates. Use the issuing CA,
  not the leaf certificate, so certificate renewal does not break the source.
* Plaintext LDAP is accepted only for `localhost`/`127.0.0.0/8` development,
  because the bind password would otherwise cross the network in cleartext.

### Search bases and attributes

Both search bases must equal `baseDN` or be a descendant of it (for example
`ou=people,dc=example,dc=com`). This keeps the future read path inside the
tree the operator selected instead of trusting a second, unrelated suffix.

The attribute fields are mapping hints. Generic LDAP uses `uid`, `uidNumber`,
`gidNumber` and `member`; Active Directory (phase 2) needs different names
(`sAMAccountName`, `objectSid`-derived ids, `member`/`memberOf`). No live
lookup uses these fields in phase 1.

## UID/GID mapping

POSIX identity is what Samba and NFS actually use, so external accounts will
eventually need stable numeric ids before they can appear in shares:

* Internal users are allocated from uid **10001** upward and use primary gid
  **10000** (`naslos_users`). External mappings must not reuse these ids.
* Phase 1 only records which attributes carry the external uid/gid. A later
  phase must define an idmap range (or require the external directory to carry
  POSIX attributes), check collisions with internal users/groups, and keep the
  mapping stable across restarts.
* External accounts remain read-only: password changes and account mutations
  must never be written back to the external directory.

## Coexistence and migration

The safe migration path is additive and reversible:

1. **Phase 1 (this change):** configure and validate the provider; keep it
   disabled unless exercising the parser. Internal LDAP is unchanged.
2. **Phase 2:** add read-only lookup and a deterministic merge policy, then AD
   domain join (Samba winbind/idmap). Internal users and groups keep their ids;
   external users get a separate, collision-checked id range.
3. **Phase 3:** Kerberos keytabs and NFS `sec=krb5`, only after the serving
   layer supports it.

Because the internal directory remains writable and authoritative, switching
back is a configuration change: disable the external block and the internal
path is exactly as it was.

## Why Kerberos cannot be enabled yet

NFS is served by **NFS-Ganesha in userspace**, not the kernel, because Talos
ships no `nfsd`. The rendered export currently pins:

```
NFS_KRB5      { Active_krb5 = false; }
...
SecType       = sys;
```

That is `AUTH_SYS` with numeric uid/gid. `sec=krb5` (and `krb5i`/`krb5p`)
requires three things that do not exist in the current stack:

1. Kerberos support in the serving layer (Ganesha built/configured with krb5,
   plus a keytab and realm configuration) — the Naslos image and rendered
   config do not provide it;
2. a KDC/realm and per-service keytabs with a renewal path that survives pod
   restarts;
3. a client-side idmap and NFS mount options that match the exported security
   flavor.

Until all three are implemented and drilled, advertising Kerberos would produce
mounts that fail or silently fall back. The UI and docs must therefore state
`AUTH_SYS` only, and `FR-EXT-12` stays `[OPEN]`.

## What remains OPEN

* Live read-only identity lookup and outage semantics (FR-EXT-07).
* Deterministic merge/collision policy with internal identities (FR-EXT-08).
* Samba account mirror and share-access integration for external identities
  (FR-EXT-09).
* Authelia login for external identities without locking out internal admins
  (FR-EXT-10).
* Active Directory domain join, winbind/idmap and the management UI
  (FR-EXT-11).
* Kerberos keytabs and NFS `sec=krb5` (FR-EXT-12).
* No UI, API route, password-change path or Samba mirror behavior exists for
  external identities in phase 1.
