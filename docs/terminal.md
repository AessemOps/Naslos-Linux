# Web terminal

A shell in the browser, for administration the UI does not (yet) cover. It is
**not** a host shell: Talos has no shell by design, so the terminal attaches to a
container that is privileged and has the host mounted, and node work happens from
inside it.

## How it is put together

```
browser (xterm.js)
   │  wss://<host>/api/ws/exec?namespace=naslos&pod=naslos-terminal-…&shell=bash
   ▼
naslos-ui (nginx)          ← WebSocket upgrade headers (location /api/ws/)
   ▼
naslos-api                 ← validates the target, then exec via the Kubernetes API
   ▼
naslos-terminal pod        ← privileged container, /host + /var/mnt + /dev mounted
   └─ bash -l              (login shell, so /etc/profile.d/naslos-shell.sh applies)
```

Components:

| Piece | Where | Notes |
| --- | --- | --- |
| `naslos-terminal` Deployment | `charts/naslos/templates/terminal.yaml` | One idle container (`sleep infinity`); exec sessions attach to it. `terminal.enabled=false` removes it and the API's RBAC together |
| exec + pickers | `api/internal/server/websocket.go`, `pods.go` | Pod/namespace listing, target validation, SPDY exec, resize queue |
| terminal page | `ui/src/routes/terminal/+page.svelte` | Namespace/pod/container pickers, shell picker, toasts for errors |
| shell setup | `terminal/image/naslos-shell.sh` | `zpool`/`zfs`/`wipefs` wrappers that chroot into the host |

## Using it

Pick the namespace and pod (the shell container is preselected) and press
Connect. Then:

```bash
zpool status            # wrapper: chroot /host /usr/local/sbin/zpool status
zfs list                # wrapper
wipefs -a /dev/vdb      # wrapper, only if the host ships wipefs
chroot /host <command>  # anything else the host has binaries for
ls /var/mnt/test        # the datasets, as the host sees them
```

The wrappers exist because the image deliberately carries no ZFS packages:
`zfsutils-linux` is in Debian's contrib component, and the host's own binaries
match the running kernel module. Note the path *after* `chroot /host` is relative
to the new root (`/usr/local/sbin/zpool`), which is a mistake worth remembering.

## Security

This container is **root and privileged**, with the host filesystem mounted. That
is inherent to the feature: a NAS terminal that cannot reach the host could not
repair a pool, mount a disk or read a config.

What the design does about it:

- **One target.** The API execs only into pods in the Naslos namespace (a
  namespaced `Role` granting `pods`, `pods/log`, `pods/exec`) and can only list
  namespaces cluster-wide - nothing else. It cannot exec into another workload,
  even one on the same node.
- **Shells, not commands.** `/api/ws/exec` accepts `bash`, `sh`, `ash`, `zsh`. Any
  other `shell` value is a 400, so the endpoint is not a "run this as root" API.
- **Origins are checked.** A browser websocket is accepted only when its `Origin`
  host matches the host it is talking to (ports ignored, because nginx forwards
  `Host` without the UI's NodePort).
- **The UI's authentication is the real boundary.** Deployed behind Traefik with
  Authelia `forwardAuth`, that is what keeps the terminal to authenticated users.
  
  ⚠️ **The UI NodePort (30080 by default, `ui.nodePort.enabled`) bypasses
  Authelia.** That is a pre-existing property of the VM layout, but with a
  privileged shell behind it the exposure is much larger. Set
  `ui.nodePort.enabled: false` (or firewall the port) on anything reachable by
  more than its owner.
- **Nothing is exposed by the terminal pod itself**: it runs no server, listens
  on no port, and is reachable only through the API's exec endpoint.

If the terminal is not wanted at all, set `terminal.enabled: false`: the shell
container and the exec RBAC are removed, and the page reports that nothing is
deployed to attach to.

## Protocol notes

- Keystrokes are sent as **binary** frames, control messages (resizes) as **JSON
  text** frames. Splitting by frame type is what stops a resize from being typed
  into the shell.
- The terminal sends its size on connect and on every resize; without that the
  remote TTY stays 80x24 and full-screen tools (`top`, `less`, `vim`) draw into
  the wrong shape.
- Before opening the socket the page makes a plain `GET` (no upgrade) to the same
  URL: the API answers with the resolved target, or the reason it cannot attach.
  A browser cannot read the status of a failed handshake, so this is the only way
  to show "pod not found" or "pick a container" properly.
- An idle session is kept open by nginx's `proxy_read_timeout 3600s` on
  `/api/ws/`; closing the tab, or Disconnect, ends the exec session.
