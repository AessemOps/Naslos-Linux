# NasOS Authentication & User Management

## Overview

NasOS uses a **shared-password single sign-on** architecture where one password grants access to both the web interface and file shares (SMB).

## Architecture

```
User → Traefik (IngressRoute) → Authelia (forwardAuth) → NasOS UI/API
                                ↓
                         OpenLDAP (identity store)
                                ↓
                         Samba passdb (SMB, synced)
```

### Components

- **Traefik**: Ingress controller with IngressRoute CRDs and forwardAuth middleware
- **Authelia**: SSO portal, handles sessions, TOTP/WebAuthn 2FA, access control rules
- **OpenLDAP**: Identity store (persons, groups) with LDAPS
- **Samba**: File sharing with tdbsam passdb, NT hashes synced from OpenLDAP password changes

## Shared Password Flow

The key invariant: **all password changes flow through the NasOS API**, which updates both stores atomically.

### Password Change Flow

1. User changes password via NasOS UI (or admin sets it)
2. NasOS API calls `identity.SetPassword(uid, password)`:
   - LDAP Password Modify extended operation (RFC 3062) → directory hashes and stores
   - Compute NT hash (MD4 of UTF-16LE password)
   - Sync NT hash to Samba passdb via `pdbedit --set-nt-hash`
3. Both stores now reflect the new password
4. Web login: Authelia → OpenLDAP LDAP bind (validates userPassword hash)
5. SMB login: Samba → tdbsam (validates NT hash)

### Why This Works

- LDAP `userPassword`: Hashed by the directory (SSHA/Argon2), verified by LDAP bind
- SMB NT hash: Pre-computed from the same plaintext password, stored in Samba passdb
- Both derived from the same password → user experiences single sign-on

## Security Model

### Header Trust

Authelia sets `Remote-User`, `Remote-Groups`, `Remote-Email`, `Remote-Name` headers.
The NasOS API **trusts these headers ONLY from Traefik's pod CIDR** (configured via `TRAEFIK_CIDR` env var).

This is the **most critical security boundary**: an attacker who can reach the API directly (bypassing Traefik) cannot spoof authentication headers.

### Access Control

Two default groups:
- **nasos_admins**: Full access (user management, app management, disk management)
- **nasos_users**: Read-only access (view-only)

Authelia rules:
- `/api/health`: Bypass (no auth, for health checks)
- `/api/auth/*`: One factor (authenticated users)
- `/api/users`, `/api/groups`, `/api/apps`, `/api/volumes`, `/api/shares`: Two factor (admins)

### TLS

- LDAPS (LDAP over TLS) for directory connections
- HTTPS for all web traffic (Traefik terminates TLS)
- Internal CA generated at install time

## Deployment

### 1. Install CRDs

```bash
# Traefik CRDs (required before first install)
helm show crds traefik/traefik | kubectl apply --server-side --force-conflicts -f -

# Install NasOS
helm install nasos ./charts/nasos -n nasos --create-namespace
```

### 2. Bootstrap

After first install, the bootstrap Job:
- Sets the service account password
- Verifies groups are created
- Reports success

### 3. Create First Admin

```bash
# Port-forward to access the API
kubectl port-forward -n nasos svc/nasos-api 8080:8080

# Create first user via API
curl -X POST http://localhost:8080/api/users \
  -H 'Content-Type: application/json' \
  -d '{
    "uid": "admin",
    "firstName": "Admin",
    "lastName": "User",
    "email": "admin@nasos.local",
    "password": "SecurePassword123!",
    "groups": ["nasos_admins"]
  }'
```

## Backup & Restore

OpenLDAP is backed up via the `nasos-openldap-backup` CronJob:
- Runs daily at 03:00 UTC
- Exports config and data via `slapcat`
- Stores on ZFS-backed PVC (snapshots for point-in-time recovery)
- Retains last 7 backups

### Manual Backup

```bash
kubectl exec -n nasos deploy/nasos-openldap -- slapcat -n 1 > backup.ldif
```

### Restore

```bash
# Stop writes, restore, restart
kubectl exec -n nasos deploy/nasos-openldap -- slapadd -n 1 -l backup.ldif
```

## Troubleshooting

### Cannot log in via web

1. Check Authelia logs: `kubectl logs -n nasos deploy/authelia`
2. Verify LDAP connectivity: check `LDAP_HOST`, `LDAP_BIND_PASS` env vars
3. Test LDAP bind: `ldapwhoami -H ldaps://nasos-openldap:636 -D "uid=user,ou=people,dc=nasos,dc=local" -W`

### Cannot access SMB shares

1. Check Samba logs
2. Verify NT hash sync: check NasOS API logs for "Syncing SMB password"
3. Test with `smbclient`: `smbclient //nasos/share -U user`

### Password changes not taking effect

1. Check both stores: LDAP (`ldapsearch`) and Samba (`pdbedit --list`)
2. Verify the NasOS API connected to both LDAP and Samba
3. Check for sync errors in API logs
