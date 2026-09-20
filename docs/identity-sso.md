# Identity & Single Sign-On

Naslos uses a **shared-password SSO**: one password grants access to both the
web interface (Authelia → OpenLDAP) and file shares (Samba). All password
changes flow through the Naslos API, which updates both stores atomically.

```
 User → Traefik (IngressRoute) → Authelia (forwardAuth) → Naslos UI / API
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
- **Samba** — file sharing (`naslos-samba` DaemonSet, `hostNetwork` :445) with
  a `tdbsam` passdb whose NT hashes are mirrored from LDAP password changes.

## Directory layout

```
dc=naslos,dc=local
├── ou=people      (inetOrgPerson + posixAccount + shadowAccount)
├── ou=groups      (groupOfNames: naslos_admins, naslos_users, …)
└── ou=services    (cn=naslos-service — the API's LDAP bind account)
```

Users are created with objectClasses `inetOrgPerson`, `posixAccount`,
`shadowAccount`; enabling/disabling maps to `shadowExpire` (`-1` = enabled,
`1` = disabled).

## Shared Password Flow

1. User changes password via the Naslos UI (or admin sets it).
2. `POST /api/users/{uid}/password` → `identity.SetPassword(uid, password)`:
   - LDAP **Password Modify** extended operation (RFC 3062) → the directory
     hashes and stores `userPassword`;
   - compute the NT hash (MD4 of the UTF-16LE password) from the submitted
     plaintext — LDAP's salted hash cannot yield it later;
   - record it in the SMB account mirror
     (`shares.SambaUserStore`, `smbusers.json` on the API's PVC) together with
     the user's `uidNumber` read from LDAP;
   - render the mirror as an `smbpasswd`-format file, push it through the
     privileged **naslos-agent** to `/var/lib/naslos/shares/smbusers` on the
     node, where the naslos-samba container imports it with
     `pdbedit -i smbpasswd:<file>`;
   - also mirror the POSIX identity: read `uidNumber`/`gidNumber` from the LDAP
     entry and render the `extrausers` files (`passwd`, `group`, `shadow`) that
     the samba container resolves through NSS. Samba attaches a session to a
     UNIX uid, so this is what makes the login work with **no account created
     on the node** and no LDAP credentials in the serving container.
3. Both stores now reflect the new password.
4. Web login: Authelia → LDAP bind validates `userPassword`.
5. SMB login: Samba → its passdb validates the NT hash.

The same triggers fire on user create (with a password), delete (account
removed) and enable/disable (account flagged `[DU]`, hash retained). Verified
end-to-end: a user created through the API can immediately log into SMB and
read/write a ZFS dataset, with no local account on the node.

Timing: the API call itself is synchronous and returns in ~25 ms, but the
serving container picks the change up on its next poll, so a **new** SMB
connection accepts the new password within ~3 s (tunable down to ~1 s with
`shares.confCheckInterval`). Existing SMB sessions keep their credentials until
they reconnect — see [shares.md](shares.md#how-fast-a-password-change-applies).

Note the NT hash is *not* stored in LDAP: that would require the Samba schema
(`sambaNTPassword`, `sambaSamAccount`) and is a possible future change. Today
the directory remains the identity source of truth and the SMB mirror lives
alongside the share definitions.

See [shares.md](shares.md#account-synchronisation-smb--ldap) for the rendering
format and for the two pitfalls that silently break SMB logins
(`SMB_CONF_PATH`, and the need for a resolvable POSIX account).


## Header Trust

Authelia sets `Remote-User`, `Remote-Groups`, `Remote-Email`, `Remote-Name`
headers. The Naslos API trusts them only when **both** hold (NAS-001):

1. the request comes from Traefik's pod CIDR (`TRAEFIK_CIDR`, default
   `10.0.0.0/8`), and
2. it carries the shared secret that Traefik's `proxy-identity` middleware
   injects (`X-Naslos-Proxy-Secret`, generated once into the `naslos-proxy`
   Secret). A client that reaches the API another way — another pod, a stale
   listener — cannot supply it, so spoofing `Remote-User` alone is not enough.

Traefik's forwardAuth middleware does **not** set `trustForwardHeader`: a client
must not be able to forge `X-Forwarded-*` and influence the auth decision or the
API's websocket origin check (NAS-009). `authelia_url` is pinned in the middleware
address so the portal redirect does not depend on `X-Forwarded-Host`.

AuthZ is enforced by `auth.Middleware`:

- `RequireAuth` — source IP ∈ `TRAEFIK_CIDR` **and** the proxy secret **and** a
  `Remote-User` present.
- `RequireAdmin` — additionally the user must be in group `naslos_admins`.

## Access control

Two default groups:

| Group | Access |
| --- | --- |
| `naslos_admins` | Full access (users, apps, disks, shares) |
| `naslos_users` | Read-only (view only) |

Authelia rules (`charts/naslos/templates/authelia-config.yaml`):

| Resource | Policy |
| --- | --- |
| `/api/health` | bypass (health checks) |
| `/api/buddy/v1/` | bypass (peers authenticate with their own keys, not a session) |
| `/authelia` | bypass (the portal route has no forwardAuth) |
| every path, for a user in `group:naslos_admins` | two_factor (2FA on the whole surface, so the portal prompts enrolment during the first login — not just on the API prefixes, which would let the SPA open on a one-factor session) |
| `/traefik` for `group:naslos_users` | deny (the Traefik dashboard is operator-only; admins matched the rule above and keep 2FA) |
| everything else | one_factor (authenticated; the API's `RequireAdmin` group check still gates admin-only routes) |

The admin rule is subject-based (`group:naslos_admins`) rather than a list of
paths. The API checks group membership, not the factor, so if only some paths
were two_factor an admin could open a one-factor session and reach the rest —
including the terminal. Applying two_factor to the whole surface for admins also
means the portal asks for the second factor at first login, instead of letting
the SPA load and then bouncing the admin API calls (which reads as a JSON error
in the page).

## 2FA & sessions

- TOTP (SHA1/6 digits/30 s) and WebAuthn are enabled.
- Sessions: 1 h expiry, 5 min inactivity, 1 M "remember me"; stored in local
  SQLite (`/config/db.sqlite3`) for single-node.
- Regulation: 3 retries → 2 min window → 5 min ban.
- Password policy: ≥8 chars ≤72, upper + lower + number required.
- Authelia's own password reset is disabled; resets happen in the Naslos UI → LDAP.
- **Elevated session / identity verification.** Registering or removing a second
  factor is a credential-management action, so Authelia requires an elevated
  session whose one-time code is delivered by the notifier. There is no mail
  relay on the appliance, so the notifier is `filesystem`
  (`/config/notification.txt`, 0600 on the Authelia volume) and the code has a
  1 h lifespan. Set the account's `mail` attribute first (Authelia addresses the
  code to it) — `admin` uses `admin@naslos.local`. During the one-time
  enrolment, read the code with
  `kubectl -n naslos exec daemonset/naslos-authelia -- cat /config/notification.txt`.
  `skip_second_factor: true` means that once an operator has a second factor,
  later credential changes only need that factor, not another code. Configure
  `notifier.smtp` instead if a relay exists.

## TLS

- LDAPS (:636) with an internal CA; the CA is provided to the API
  (`LDAP_CA_CERT`) and to Authelia via the config map and secrets.
- HTTPS terminated by Traefik (`websecure` :443, HTTP→HTTPS redirect), exposed on
  the node's 80/443 (`values-vm.yaml` sets `traefik.ports.*.hostPort`).
- The certificate is chart-generated, self-signed for the `domain` value
  (`naslos-tls`, generated once and reused across upgrades). Point
  `ingress.tls.existingSecret` at a real certificate to remove the browser
  warning, or import the generated `tls.crt` on the client.
- The Authelia portal is served at `https://naslos.local/authelia`; the
  forwardAuth authz URL stays at the root (`/api/authz/forward-auth`) as
  Authelia serves both paths.
- Traefik's own dashboard is served at `https://naslos.local/traefik/dashboard/`
  (`api.basePath: /traefik`, `api.insecure` off, so the IngressRoute is the only
  way in) and is admin-only — the sidebar shows the link only to `naslos_admins`.
- Security headers: XSS filter, nosniff, frame deny, HSTS, referrer policy (no
  `preload`: a `.local` self-signed host cannot earn it and it would remove the
  "proceed anyway" path).

## Third-party apps & LDAP

The same directory can back apps (e.g. Plex, Nextcloud) configured with the
OpenLDAP server (`ldaps://naslos-openldap:636`, `dc=naslos,dc=local`) and the
`naslos-service` bind account — avoiding per-app user databases.

## Deployment & first admin

1. Install CRDs, install the chart (see [deployment.md](deployment.md)).
2. The OpenLDAP bootstrap Job sets the service password + verifies groups. It
   does **not** create a human user, and `/api/users` is an owner route — so a
   bare `kubectl port-forward` POST is now rejected with `401`. The request must
   carry the `X-Naslos-Proxy-Secret` (Secret `naslos-proxy`, key `secret`) and
   `Remote-User`/`Remote-Groups`, and come from a source the API's
   NetworkPolicy allows (the Traefik/UI pods, or any pod in
   `naslos-privileged`). For example, from the terminal pod:

```bash
SECRET=$(kubectl -n naslos get secret naslos-proxy -o jsonpath='{.data.secret}' | base64 -d)
kubectl -n naslos-privileged exec deploy/naslos-terminal -- \
  curl -sS -X POST http://naslos-api.naslos.svc.cluster.local:8080/api/users \
    -H "X-Naslos-Proxy-Secret: $SECRET" \
    -H 'Remote-User: admin' -H 'Remote-Groups: naslos_admins' \
    -H 'Content-Type: application/json' \
    -d '{
      "uid": "admin", "firstName": "Admin", "lastName": "User",
      "email": "admin@naslos.local",
      "password": "SecurePassword123!",
      "groups": ["naslos_admins"]
    }'
```

Alternatively, `ldapadd` the person entry directly and add it to
`cn=naslos_admins` (the API and the Samba mirror pick it up from LDAP). After
the admin exists, log in normally through `https://naslos.local/authelia`.

## Backup, restore & troubleshooting

Moved to [operations.md](operations.md#openldap-backup) — includes the nightly backup
CronJob, manual `slapcat`/`slapadd`, and the login/SMB/password-sync checklists.

## UI management (Users & Groups pages)

The Naslos UI (`/users` and `/groups`) provides full CRUD management:

- **Users page**: lists all users in a table with display name, email, group
  badges, status (Active/Disabled), and actions (Edit, Enable/Disable, Delete).
  The Enable/Disable toggle maps to `shadowExpire` on the LDAP `shadowAccount`
  objectClass (`-1` = enabled, `1` = disabled).
- **Groups page**: lists all groups as cards with member count and member badges.
  Groups can be edited to add/remove members.
- **Table alignment**: the users table uses `table-layout: fixed` with explicit
  column widths and `align-top` so multi-line group badges don't misalign rows.
- **Short names**: group and user references are displayed as short names
  (e.g. `naslos_users`) instead of full DNs (e.g.
  `cn=naslos_users,ou=groups,dc=naslos,dc=local`).
- **Empty API responses**: both `/api/users` and `/api/groups` always return
  arrays (never `null`), preventing UI freezes when LDAP has no entries.
- **Group descriptions**: optional in the UI and API. When empty, the
  `description` LDAP attribute is omitted entirely (OpenLDAP rejects empty
  string values).
- **Placeholder members**: `groupOfNames` requires at least one `member`. The API
  adds a schema-required placeholder (`cn=empty-members,ou=groups,...`) during
  creation and filters it from all responses, so users see only real members.