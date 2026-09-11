# Operations

Day-2 topics: backups, restore, common failures.

## OpenLDAP backup

`naslos-openldap-backup` CronJob:

- Runs daily at **03:00 UTC**.
- Exports config (`slapcat -n 0`) and data (`slapcat -n 1`) to
  `/var/lib/ldap/backups` (ZFS-backed PVC → snapshots give point-in-time).
- Retains the last **7** backups.

### Manual backup

```bash
kubectl exec -n naslos deploy/naslos-openldap -- slapcat -n 1 > backup.ldif
```

### Restore

```bash
# Stop writes, restore, restart
kubectl exec -n naslos deploy/naslos-openldap -- slapadd -n 1 -l backup.ldif
```

(Stop the API or put LDAP in read-only mode first; the API reconnects
automatically on the next operation.)

## Troubleshooting

### Cannot log in via web

1. Check Authelia logs: `kubectl logs -n naslos deploy/authelia`.
2. Verify `LDAP_HOST`, `LDAP_BIND_PASS`, and the Authelia LDAP config map
   (`LDAP_BIND_DN`/`LDAP_BIND_PASS` under `authentication_backend.ldap`).
3. Test a bind directly:
   ```bash
   ldapwhoami -H ldaps://naslos-openldap:636 \
     -D "uid=user,ou=people,dc=naslos,dc=local" -W
   ```
4. Check TOTP/WebAuthn registration for 2FA users; check the regulation ban
   (`max_retries: 3`, 5 min ban).

### Cannot access SMB shares

1. Check Samba logs (`/var/log/samba/%m.log`).
2. Verify NT-hash sync: API logs should show "Syncing SMB password for …".
3. Test: `smbclient //naslos/share -U user`.
4. Ensure the share path exists under `/var/mnt` and the ZFS dataset is
   mounted (`zpool status`, `zfs list`).

### Password changes not taking effect

1. Check both stores:
   - LDAP: `ldapsearch … uid=<user> userPassword` (hash changed?)
   - Samba: `kubectl exec … pdbedit --list` (NT hash changed?)
2. Confirm the API can reach LDAP and can exec into the Samba container
   (`kubectl exec` RBAC — the API's service account must have `pods/exec`).
3. Look for sync errors in API logs.

### ZFS pool not imported after reboot

- Confirm the ZFS extension is installed: on the node,
  `ls /host/usr/local/sbin/zpool`.
- Check `zfs-service` ran: `zpool import -fal` output in node system logs.
- If the pool is foreign, clear properties and re-import:
  ```bash
  zpool import -f <pool>; zfs set context=none … <pool>
  ```

### Agent unreachable

- Agent runs as DaemonSet `hostNetwork`; confirm it's on the same host as the
  pool: `kubectl get daemonsets -n naslos`.
- API reaches the agent via the node address (`:9090`); check K8s network
  policy / RBAC (`naslos-agent` ClusterRole grants node/pod read).

## Reference

- [archive: architecture](architecture.md) for trust boundaries.
- [identity-sso.md](identity-sso.md) for the password path and header trust.