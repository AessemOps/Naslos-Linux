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

`slapadd` must never run against a live slapd: it rewrites the mdb database
files underneath the running server and corrupts the directory. The restore is
offline end to end: capture a fresh dump, stop the StatefulSet, restore into
the (now exclusively owned) data volume from a one-off pod, then start it
again. The API reconnects automatically on its next operation; users cannot log
in (and the UI's authenticated calls fail) while slapd is down.

1. **Take a fresh dump first.** A manual Job copied from the backup CronJob
   runs `slapcat` against the live server and mounts the same PVCs; it must run
   before the StatefulSet is stopped. After the scale-down there is no slapd
   pod left to `kubectl exec` into, so the backup cannot be taken then.

```bash
BACKUP_JOB="openldap-backup-restore-$(date +%s)"
kubectl -n naslos create job --from=cronjob/naslos-openldap-backup "$BACKUP_JOB"
kubectl -n naslos wait --for=condition=complete "job/$BACKUP_JOB" --timeout=300s
```

2. **Stop the only writer.**

```bash
kubectl -n naslos scale statefulset/naslos-openldap --replicas=0
kubectl -n naslos wait --for=delete pod/naslos-openldap-0 --timeout=120s
```

3. **Run the restore pod** with the same two PVCs the StatefulSet uses plus the
   backups PVC. `slapadd` must run as the image's `openldap` user so the created
   files keep the ownership slapd expects:

```bash
kubectl -n naslos apply -f - <<'EOF'
apiVersion: v1
kind: Pod
metadata:
  name: naslos-openldap-restore
spec:
  restartPolicy: Never
  securityContext:
    runAsUser: 101
    runAsGroup: 101
    fsGroup: 101
    runAsNonRoot: true
  containers:
    - name: restore
      image: ghcr.io/aessemops/naslos-openldap:<version>
      command: ["sleep", "infinity"]
      securityContext:
        allowPrivilegeEscalation: false
        readOnlyRootFilesystem: true
        capabilities:
          drop: [ALL]
      volumeMounts:
        - name: data
          mountPath: /var/lib/ldap
        - name: config
          mountPath: /etc/ldap/slapd.d
        - name: backups
          mountPath: /backups
  volumes:
    - name: data
      persistentVolumeClaim:
        claimName: data-naslos-openldap-0
    - name: config
      persistentVolumeClaim:
        claimName: config-naslos-openldap-0
    - name: backups
      persistentVolumeClaim:
        claimName: naslos-openldap-backups
EOF

# 4. Archive the old mdb environment. slapadd -n 1 must write a fresh
#    database: it rejects duplicate DNs, would otherwise keep entries deleted
#    since the backup, and cannot open a corrupt old environment. The moved
#    files are the rollback point if the new dump turns out to be wrong.
kubectl -n naslos exec naslos-openldap-restore -- sh -c '
  cd /var/lib/ldap
  for f in data.mdb lock.mdb; do
    if [ -f "$f" ]; then mv "$f" "$f.pre-restore-<timestamp>"; fi
  done'

# 5. Restore the data.
kubectl -n naslos exec naslos-openldap-restore -- \
  slapadd -F /etc/ldap/slapd.d -n 1 -l /backups/ldap/data_<timestamp>.ldif

# 6. Start the server again and remove the helper pod.
kubectl -n naslos delete pod naslos-openldap-restore
kubectl -n naslos scale statefulset/naslos-openldap --replicas=1
kubectl -n naslos rollout status statefulset/naslos-openldap
```

**Only when `cn=config` was lost** (a data-only restore against a valid config
volume is the normal case) restore the config *before* step 4: `slapadd -n 0`
also refuses existing entries, so archive the old config tree first, then load
the config dump and continue with the data steps above.

```bash
kubectl -n naslos exec naslos-openldap-restore -- sh -c '
  mkdir -p /backups/ldap/config-pre-restore-<timestamp>
  find /etc/ldap/slapd.d -mindepth 1 -maxdepth 1 \
    -exec mv {} /backups/ldap/config-pre-restore-<timestamp>/ \;'
kubectl -n naslos exec naslos-openldap-restore -- \
  slapadd -F /etc/ldap/slapd.d -n 0 -l /backups/ldap/config_<timestamp>.ldif
```

**Rollback:** before scaling up, delete the files the restore created
(`data.mdb`, `lock.mdb`, and the restored `cn=config` tree if the config restore
ran) and move the `*.pre-restore-<timestamp>` archives back into place. The
`naslos-openldap-backups` PVC keeps the dumps, not the old environment.

### Sizing and limits

`openldap.dataSize`, `openldap.configSize` and `openldap.storageClassName`
are install-time settings: the StatefulSet's `volumeClaimTemplates` create the
PVCs once, so editing them on an existing install does not resize or move the
volumes (grow the PVC with its storage class first). The mdb map limit is also
fixed at directory initialisation: `olcDbMaxsize` is 1 GiB in
`openldap/image/entrypoint.sh`, so a larger `dataSize` alone does not raise
the database limit.

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

### Rebooting the node (not an upgrade)

Settings → **System** → **Reboot node** asks for `reboot` and then runs
`talosctl reboot` through the API. Shares and apps are offline until the node
returns; the page polls `/api/system/version` and shows "node rebooting,
reconnecting…". Unlike the upgrade operations this works on `make install-vm`
nodes too and does not write the operation ConfigMap.

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
- **`Apply failed with 1 conflict` on a Naslos upgrade**: the release was
  first installed by the desktop engine, whose server-side apply owns fields
  this release changes. The upgrader forces conflicts on `helm upgrade` and
  `helm rollback`; an older release without that flag must be upgraded to a
  build that has it. The Helm history description carries the full conflict
  list (`helm -n naslos history naslos --max 2`).
- **Talos A/B rollback**: Talos boots the previous slot automatically. Confirm
  with `talosctl -n <node> get version`; the operation reports `failed` with
  "rolled back". Fix the cause (image/schematic) before retrying.
- **Failed `upgrade-k8s`**: there is no automatic rollback. Check
  `kubectl get nodes -o wide` and
  `kubectl -n kube-system get pods`; re-run
  `talosctl upgrade-k8s --to <version>` (it is idempotent) or restore the etcd
  snapshot from `/var/lib/naslos/backups/etcd-<ts>.db` on the node if the
  control plane does not return. Keep the three newest snapshots.

## Network recovery

Settings → Network applies changes with a machine-side `--mode=try` guard
(60 s) and an API watchdog (5 min). If a change leaves the node unreachable and
both guards failed, recover the saved configuration instead of reinstalling:

1. **Read the saved snapshot.** It is the `pending.previousConfig` field in
   `NETWORK_STATE` (`/var/lib/naslos/network.json` on the `naslos-shares-config`
   PVC). With a running API pod:
   ```bash
   kubectl -n naslos exec deploy/naslos-api -- cat /var/lib/naslos/network.json
   ```
   If the API pod cannot start, mount the PVC in a debug pod and read the same
   file.
1a. **MCP state** lives beside it: `/var/lib/naslos/mcp.json` on the same PVC
   (`MCP_CONFIG`) holds the enabled flag and the token hashes. The same
   debug-pod read works when the API is down, and an operator-side
   `"enabled": false` edit is the emergency kill switch for the MCP endpoint
   when the UI is unreachable.
2. **Apply the snapshot from a workstation** that has the admin talosconfig:
   save the `previousConfig` string as `previous.yaml`, then
   ```bash
   talosctl apply-config --file previous.yaml --nodes <node> --mode=no-reboot
   ```
   `no-reboot` is deliberate: the snapshot restores the working link state
   immediately.
3. **Console / ISO fallback.** If the Talos API is also unreachable, boot the
   node's maintenance ISO or use the console and apply the same file with
   `talosctl apply-config`. Only after the node is reachable again should you
   diagnose from the UI.

The change record is a crash-recovery aid, not a backup: it holds one pending or
just-finished change. The install-time machine config remains in
`bootstrap/vm/controlplane.yaml` and the installer recovery ZIP.

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
- The agent imports with `zpool import -N`, remaps every stored mountpoint to
  `/var/mnt/<pool>/…`, and mounts deliberately. A forced import is never
  automatic: it needs `{force:true, confirm:"<pool>"}` on
  `POST /api/volumes/zfs/import`. If you must do it by hand on the node, keep
  `-N` and inspect `zfs get mountpoint` before mounting:
  ```bash
  zpool import -N -f <pool>
  zfs list -H -o name,mountpoint,canmount -r <pool>   # remap anything outside /var/mnt/<pool>
  zfs set context=none … <pool>
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

## App storage pools

New installs can put the chart's config/database PVCs on a ZFS pool. In the
install dialog choose **Storage pool**; the API creates a pool-scoped
local-path provisioner in `local-path-storage` and a
`naslos-pool-<slug>-<hash>` StorageClass, then points every
`persistence.*.storageClass` at it. The provisioned PVs land under
`/var/mnt/<pool>/apps`. Leave the selector on **Default (local-path)** for the
previous behaviour. The dataset selector is separate: it controls the app's
media/data claim, not its persistence PVCs.

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
