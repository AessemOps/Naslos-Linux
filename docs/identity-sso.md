# Identity & Single Sign-On

NasOS uses a **shared-password SSO**: one password grants access to both the
web interface (Authelia → OpenLDAP) and file shares (Samba). All password
changes flow through the NasOS API, which updates both stores atomically.

```
 User → Traefik (IngressRoute) → Authelia (forwardAuth) → NasOS UI / API
                                  ↓
                           OpenLDAP (identity store)
                                  ↓
                           Samba passdb (SMB, NT-hash synced)
```

## Components

- **Traefik** — ingress controller (IngressRoute CRDs) and forwardAuth
  middleware that validates every request against Authelia.
- **Authelia** — SSO portal: sessions, TOTP + WebAuthn (2FA), access rules,
  regulation (brute-force protection).
- **OpenLDAP** — identity store (`ou=people`, `ou=groups`, `ou=services`) on
  LDAPS :636, with `memberOf` and referential-integrity overlays.
- **Samba** — file sharing with `tdbsam` passdb; NT hashes synced from LDAP
  password changes.

## Directory layout

```
dc=nasos,dc=local
├── ou=people      (inetOrgPerson + posixAccount + shadowAccount)
├── ou=groups      (groupOfNames: nasos_admins, nasos_users, …)
└── ou=services    (cn=nasos-service — the API's LDAP bind account)
```

Users are created with objectClasses `inetOrgPerson`, `posixAccount`,
`shadowAccount`; enabling/disabling maps to `shadowExpire` (`-1` = enabled,
`1` = disabled).

## Shared Password Flow

1. User changes password via the NasOS UI (or admin sets it).
2. `POST /api/users/{uid}/password` → `identity.SetPassword(uid, password)`:
   - LDAP **Password Modify** extended operation (RFC 3062) → directory hashes
     and stores `userPassword`;
   - compute the NT hash (MD4 of the UTF-16LE password);
   - sync the NT hash to Samba's passdb via
     `kubectl exec … pdbedit --set-nt-hash <uid> <hash>`.
3. Both stores now reflect the new password.
4. Web login: Authelia → LDAP bind validates `userPassword`.
5. SMB login: Samba → tdbsam validates the NT hash.

## Header Trust

Authelia sets `Remote-User`, `Remote-Groups`, `Remote-Email`, `Remote-Name`
headers. The NasOS API **trusts these only from Traefik's pod CIDR**
(`TRAEFIK_CIDR` env var; default `10.0.0.0/8` — **tighten this in production**).

> This is the most critical security boundary. An attacker who can reach the
> API directly (bypassing Traefik) cannot spoof auth headers.

AuthZ is enforced by `auth.Middleware`:

- `RequireAuth` — source IP ∈ `TRAEFIK_CIDR` **and** a `Remote-User` present.
- `RequireAdmin` — additionally the user must be in group `nasos_admins`.

## Access control

Two default groups:

| Group | Access |
| --- | --- |
| `nasos_admins` | Full access (users, apps, disks, shares) |
| `nasos_users` | Read-only (view only) |

Authelia rules (`charts/nasos/templates/authelia-config.yaml`):

| Resource | Policy |
| --- | --- |
| `/api/health` | bypass (health checks) |
| Authelia's own endpoints | bypass |
| everything else | one_factor (authenticated) |
| `/api/users`, `/api/groups`, `/api/apps`, `/api/volumes`, `/api/shares` | two_factor (admin; requires 2FA) |

## 2FA & sessions

- TOTP (SHA1/6 digits/30 s) and WebAuthn are enabled.
- Sessions: 1 h expiry, 5 min inactivity, 1 M "remember me"; stored in local
  SQLite (`/config/db.sqlite3`) for single-node.
- Regulation: 3 retries → 2 min window → 5 min ban.
- Password policy: ≥8 chars ≤72, upper + lower + number required.
- Authelia's own password reset is disabled; resets happen in the NasOS UI → LDAP.

## TLS

- LDAPS (:636) with an internal CA; the CA is provided to the API
  (`LDAP_CA_CERT`) and to Authelia via the config map and secrets.
- HTTPS terminated by Traefik (`websecure` :443, HTTP→HTTPS redirect).
- Security headers: XSS filter, nosniff, frame deny, HSTS, referrer policy.

## Third-party apps & LDAP

The same directory can back apps (e.g. Plex, Nextcloud) configured with the
OpenLDAP server (`ldaps://nasos-openldap:636`, `dc=nasos,dc=local`) and the
`nasos-service` bind account — avoiding per-app user databases.

## Deployment & first admin

1. Install CRDs, install the chart (see [deployment.md](deployment.md)).
2. The OpenLDAP bootstrap Job sets the service password + verifies groups.
3. Create the first admin via the API:

```bash
kubectl port-forward -n nasos svc/nasos-api 8080:8080
curl -X POST http://localhost:8080/api/users \
  -H 'Content-Type: application/json' \
  -d '{
    "uid": "admin", "firstName": "Admin", "lastName": "User",
    "email": "admin@nasos.local",
    "password": "SecurePassword123!",
    "groups": ["nasos_admins"]
  }'
```

## Backup, restore & troubleshooting

Moved to [operations.md](operations.md#openldap-backup) — includes the nightly backup
CronJob, manual `slapcat`/`slapadd`, and the login/SMB/password-sync checklists.