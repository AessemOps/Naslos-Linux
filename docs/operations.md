# Operations

Day-2 topics: backups, restore, common failures.

## OpenLDAP backup

`naslos-openldap-backup` CronJob:

- Runs daily at **03:00 UTC**.
- Exports config (`slapcat -F /etc/ldap/slapd.d -n 0`) and data (`… -n 1`) to
  `/backups/ldap` on a dedicated `naslos-openldap-backups` PVC (`openldap.backup.size`,
  default 2Gi). It is deliberately not the config volume: a few retained dumps
  could fill the 100 MiB volume slapd itself needs.
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

## Upgrading

### From the web UI (installer-provisioned nodes)

Settings → **System** shows the running Naslos, chart, Talos and Kubernetes
versions. **Check now** verifies the newest eligible release pack (sha256 plus
every member checksum) and offers one step at a time:

1. **Naslos** — runs the target release's API image in an upgrader Job. It
   snapshots etcd, converges local-path/cert-manager, `helm upgrade --reset-values`
   from the new pack, keeps only the installer overrides (`domain`, `sso.domains`,
   `shares.discovery.name`, `openldap.host`, `api.talosConfigSecret`, the three
   `networkPolicy` CIDRs), waits for the core workloads and verifies every image
   digest. On failure it rolls Helm back to the previous revision.
2. **Talos** — `talosctl upgrade` to the pack's pinned installer image; the node
   reboots and the operation resumes afterwards. A/B rollback is detected and
   reported as `failed`.
3. **Kubernetes** — `talosctl upgrade-k8s --to <pin>` (dry-run first). There is
   no automatic rollback.

The operation lock and progress live in the `naslos-system-operation`
ConfigMap, so an API restart or node reboot does not lose them. The UI keeps
polling and shows "node rebooting, reconnecting…" during the Talos step.

A `make install-vm` deployment is not web-upgradable; the page shows the
versions and points at `make install-vm`.

### From the CLI (bootstrap or no UI access)

Run the installer against the existing state with `--resume`:

```bash
read -rs NASLOS_ADMIN_PASSWORD && export NASLOS_ADMIN_PASSWORD
naslos-install --resume --node-ip <node> --domain <domain> --admin-user <uid> \
  --install-disk <disk> --confirm-wipe <disk>
```

`--resume` re-runs the Helm step when the deployed chart is older than the
pack's, which is the path from a 0.2.x node to the first web-upgradable release.
The admin password is only required while the `admin` step is still pending.

### Manual recovery

- **Naslos Helm rollback**:
  ```bash
  kubectl -n naslos helm history naslos
  kubectl -n naslos helm rollback naslos <previous-revision> --wait
  ```
  The upgrader does this automatically; do it by hand only if the Job itself
  failed or the API is gone.
- **Talos A/B rollback**: Talos boots the previous slot automatically. Confirm
  with `talosctl -n <node> get version`; the operation reports `failed` with
  "rolled back". Fix the cause (image/schematic) before retrying.
- **Failed `upgrade-k8s`**: there is no automatic rollback. Check
  `kubectl get nodes -o wide` and
  `kubectl -n kube-system get pods`; re-run
  `talosctl upgrade-k8s --to <version>` (it is idempotent) or restore the etcd
  snapshot from `/var/lib/naslos/backups/etcd-<ts>.db` on the node if the
  control plane does not return. Keep the three newest snapshots.

## CPU metrics

The dashboard's **CPU** card shows the model, 1/5/15-minute load averages,
temperature, and one usage bar per logical core. The API derives per-core usage
from two `/proc/stat` samples (a bounded delta) and reads frequency from
`/proc/cpuinfo`. The collector runs every 5 s (`METRICS_INTERVAL_SECONDS`).

- **Temperature is `—` / "not exposed on this node"** when the node has no CPU
  hwmon (`coretemp`/`k10temp`/`zenpower`) or thermal zone. The Talos VM has no
  sensor; physical hardware normally does. An absent sensor is not an error.
- The package temperature is preferred from a CPU hwmon; a generic thermal zone
  is only a fallback.
- Per-core usage is a short delta (about 200 ms), not the since-boot average, so
  the bars move with current load.

## Disk health

**Disks → Disk Health** (`/disks/health`) shows every whole disk with its SMART
health and, for pool members, the ZFS vdev state and read/write/cksum counters.
It is read-only and refreshes every 60 s.

- **SMART unavailable / standby** is normal on virtio disks (the VM), USB
  bridges and some RAID controllers, and for a disk that was asleep when polled.
  The ZFS half remains visible; the page never wakes a sleeping disk.
- **FAILED** or a vdev `FAULTED`/`OFFLINE`/`UNAVAIL`/`REMOVED` is an error
  notification (`disk_failure`). Replace the disk, then let the pool resilver.
  Do not run a destructive SMART self-test on a disk that holds the only copy of
  data.
- **Temperature above 55 °C** or **wear at/above 80 %** is a warning
  (`zfs_health`). Both thresholds are chart values
  (`diskHealth.temperatureWarningC`, `diskHealth.wearWarningPercent`).
- Alerts are transition-only: a persistent condition does not repeat every
  minute, and clearing it sends one recovery notice. A removed disk is cleared
  silently.
- The agent image bundles a pinned static `smartctl` and mounts the host `/sys`
  read-only. `SMARTCTL_BIN=""` (or `agent.smartctl.enabled=false`) disables
  SMART while keeping the ZFS page.

If the page shows `SMART is unavailable on this node`, check the agent pod logs
for a smartctl error and confirm the image contains `/app/smartctl`. A real
SATA/NVMe disk should report a health status; a virtio disk will not.

## Troubleshooting

### Cannot log in via web

1. Check Authelia logs: `kubectl logs -n naslos statefulset/naslos-authelia`.
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
   CA: `LDAP_BIND_PASS_FILE` (Secret `naslos-openldap`/`service-password`, mounted at `/var/run/secrets/ldap-bind/service-password`) and
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

## Credential rotation after upgrading from 0.2.2 or earlier

Installers up to 0.2.2 passed the first administrator password and the
`naslos-proxy` shared secret as arguments of a `pods/exec` call. Talos writes
the kube-apiserver audit log at `level: Metadata`
(`bootstrap/vm/controlplane.yaml`, and the installer's generated config), and
that level records the request URI — which contains the `exec` command and its
arguments. Talos retains the audit log for 30 days.

The engine no longer does this (0.2.3): admin creation sends the values on
`curl`'s stdin and the password comes from stdin/env. Existing nodes still hold
the old values in `/var/log/audit/kube/kube-apiserver.log`, so treat both as
disclosed and rotate them after upgrading.

### Administrator password

1. Sign in to the UI as the administrator and change the password under
   **Users**.
2. If the node was exposed, also remove the TOTP device and re-enrol it:
   `kubectl -n naslos exec -it naslos-authelia-0 -- authelia storage user totp delete <uid>`
   then generate a new one (the UI/CLI prints a new otpauth URI).
3. Confirm the old password fails at the Authelia portal, then sign in again.

### Proxy shared secret

The secret is the `secret` key of the `naslos-proxy` Secret (or the Secret named
by `auth.proxySecretName`). It is consumed by Traefik's `proxy-identity`
middleware and by the API as `PROXY_SHARED_SECRET`.

1. Generate a new value, e.g. `openssl rand -base64 48`.
2. Prefer a Secret you manage out of band:
   `kubectl -n naslos create secret generic naslos-proxy --from-literal=secret="$NEW" --dry-run=client -o yaml | kubectl apply -f -`
3. Restart both consumers so they pick up the new value:
   `kubectl -n naslos rollout restart deploy/naslos-api deploy/naslos-traefik`
   (an in-place `helm upgrade` with `--set auth.proxySecret="$NEW"` also works;
   the chart reuses the existing Secret on upgrade only when the value is not
   set).
4. Expect a short forwardAuth outage while the pods roll. Verify with
   `kubectl -n naslos rollout status deploy/naslos-api`, then
   `curl -sk https://<domain>/api/health` through the proxy and one login.

Do not commit either value. The audit log is retained by Talos and cannot be
rewritten through the API; rotation is what removes its value.
