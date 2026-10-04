# Operations

Day-2 topics: backups, restore, common failures.

## OpenLDAP backup

`naslos-openldap-backup` CronJob:

- Runs daily at **03:00 UTC**.
- Exports config (`slapcat -F /etc/ldap/slapd.d -n 0`) and data (`… -n 1`) to
  `/backups/ldap` on the config PVC (a `local-path` volume — there is no ZFS
  snapshot layer for it).
- Retains the last **7** backups.

### Manual backup

```bash
kubectl exec -n naslos statefulset/naslos-openldap -- \
  slapcat -F /etc/ldap/slapd.d -n 1 > backup.ldif
```

### Restore

```bash
# Stop writes, restore, restart
kubectl exec -n naslos statefulset/naslos-openldap -- \
  slapadd -F /etc/ldap/slapd.d -n 1 -l backup.ldif
```

(Stop the API or put LDAP in read-only mode first; the API reconnects
automatically on the next operation.)

## Troubleshooting

### Cannot log in via web

1. Check Authelia logs: `kubectl logs -n naslos daemonset/naslos-authelia`.
2. Verify `LDAP_HOST` and that the Authelia bind password comes from the
   `naslos-openldap` Secret key `service-password` via
   `AUTHELIA_AUTHENTICATION_BACKEND_LDAP_PASSWORD_FILE`; the `authelia-config`
   ConfigMap no longer holds it.
3. Test a bind directly:
   ```bash
   ldapwhoami -H ldaps://naslos-openldap:636 \
     -D "uid=user,ou=people,dc=naslos,dc=local" -W
   ```
4. Check TOTP/WebAuthn registration for 2FA users; check the regulation ban
   (`max_retries: 3`, 5 min ban).

### Cannot access SMB shares

1. Check the Samba logs
   (`kubectl -n naslos-privileged logs ds/naslos-samba`; file logs under
   `/var/log/samba/%m.log`).
2. Verify the account mirror reached the node: the agent writes
   `/var/lib/naslos/shares/smbusers` (read it with
   `talosctl -n <node> read /var/lib/naslos/shares/smbusers`) and the Samba
   entrypoint logs "resolve through NSS" for each account. `GET
   /api/shares/status` reports whether the rendered config was applied.
3. Test: `smbclient //naslos/share -U user`.
4. Ensure the share path exists under `/var/mnt` and the ZFS dataset is
   mounted (`zpool status`, `zfs list`).

### Password changes not taking effect

1. Check both stores:
   - LDAP: `ldapsearch … uid=<user> userPassword` (hash changed?)
   - Samba: the `smbusers` mirror on the node (above), or
     `kubectl -n naslos-privileged exec ds/naslos-samba -- pdbedit --list`
     (NT hash changed?)
2. Confirm the API reached the agent so the mirror was re-rendered: check the
   API/agent logs and
   `curl -s https://naslos.local/api/shares/status`; the agent must be
   reachable at `naslos-agent.naslos-privileged:9090`.
3. Look for render/apply errors in the API and agent logs.

### ZFS pool not imported after reboot

- Confirm the ZFS extension is installed: on the node,
  `ls /host/usr/local/sbin/zpool`.
- Check `zfs-service` ran: `zpool import -fal` output in node system logs.
- If the pool is foreign, clear properties and re-import:
  ```bash
  zpool import -f <pool>; zfs set context=none … <pool>
  ```

### Users/Groups page says "Identity/LDAP is not reachable"

The API connects to LDAP lazily and reconnects on demand, so this is
transient by design.

1. Check LDAP is actually up: `kubectl -n naslos get pods,svc naslos-openldap`
   and `kubectl -n naslos get endpoints naslos-openldap` (a Service with no
   endpoints means the pod is not ready).
2. Check what the API thinks: `curl -s localhost:8080/api/ready` (via a
   `kubectl port-forward`, or through the ingress with a session —
   `/api/ready` is behind the 2FA gate) → `{"status":"ok","ldap":"up"|"down"}`.
3. The error body names the underlying cause (e.g. `dial tcp …: connect:
   connection refused`, or `no such host`).
4. **No API restart is required** — the next request after LDAP returns will
   succeed. Restarting the API pod is only a last resort.
5. If `ldap: down` persists while LDAP is healthy, check the API's secret and
   CA: `LDAP_BIND_PASS` (Secret `naslos-openldap`/`service-password`) and
   `LDAP_CA_CERT` (Secret `naslos-openldap-tls`).
6. Startup ordering: on a cold start the API waits briefly (best-effort,
   ~10 s) for OpenLDAP, then starts anyway. `kubectl -n naslos logs
   deploy/naslos-api -c wait-for-ldap` shows whether the wait succeeded.

> Historical note: before the lazy client, a failed startup bind set the
> identity client to `nil` permanently, so this error stuck until the API pod
> was restarted by hand. That is fixed (spec FR-IDN-11/12).

### Agent unreachable

- Agent runs as a DaemonSet in `naslos-privileged`:
  `kubectl get daemonsets -n naslos-privileged`; confirm it is on the same host
  as the pool.
- API reaches the agent through the agent Service in that namespace
  (`naslos-agent.naslos-privileged.svc.cluster.local:9090`). NetworkPolicy is
  **enforced** (Cilium, AUDIT-M4), but the agent is `hostNetwork`, so pod-level
  policy does not cover `:9090` and the shared bearer token remains the control
  on that port (the open M4 residual). The agent's old ClusterRole was removed
  (AUDIT-M5) — it has no Kubernetes API permissions at all.

## Privileged workloads and why (AUDIT-L3)

There are two namespaces, separated by AUDIT-M6: the hostNetwork/privileged
workloads (agent, samba, nfs, terminal) live in **`naslos-privileged`**, while
`naslos` holds the authenticated services. `naslos` still enforces PSA
`privileged` **by necessity** because the API mounts hostPath volumes, which
`baseline` forbids. Each privileged workload, and the reason it cannot be less:

| Workload | Privilege | Why |
|---|---|---|
| `naslos-agent` (DaemonSet) | `hostNetwork`, `privileged`, hostPaths `/`, `/dev`, `/run`, `/var` | runs `zpool`/`zfs` against the node's pools and `chroot /host` for the extrausers/smb.conf mirrors; `/dev` and `/run` carry the ZFS and udev state |
| `naslos-samba` (DaemonSet) | `hostNetwork`, `CHOWN`/`DAC_OVERRIDE`/`FOWNER`/`FSETID`/`SETGID`/`SETUID`, hostPaths share-config/extrausers/datasets | SMB must bind 445 on the node and chown files it creates inside the datasets it serves |
| `naslos-nfs` (DaemonSet) | `hostNetwork`, same capability set, hostPaths share-config/datasets | NFS must bind 2049 and hand out the datasets' real UIDs |
| `naslos-terminal` (Deployment) | `privileged`, hostPaths `/`, `/var/mnt`, `/dev` | the operator's root shell inside `chroot /host`; gated by the API's owner auth (proxy secret + admin) and it never listens on a port itself |
| `naslos-traefik` | `hostPort` 80/443 | it is the LAN entry point, so it binds the node's ports directly |
| API `fix-receive-dataset-ownership` init | `runAsUser: 0` | one-shot `chown` of the Buddy receive dataset to the API's uid (65532); the API container itself runs non-root |

Everything else — `naslos-ui` and `naslos-authelia` — runs non-root with
capabilities dropped and needs no hostPath. `naslos-api` runs
non-root too, but mounts hostPath volumes for the shares view and the buddy
receive dataset, which is why its namespace must stay `privileged`. Any future
change that adds a privileged workload should add it to this table in the same
change.

## Reference

- [archive: architecture](architecture.md) for trust boundaries.
- [identity-sso.md](identity-sso.md) for the password path and header trust.