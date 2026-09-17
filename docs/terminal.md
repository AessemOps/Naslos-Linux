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

The terminal is therefore gated on **proof of an authenticated session**, checked
in three places so that no single mistake opens it:

| Layer | Behaviour |
| --- | --- |
| `naslos-ui` nginx | **Refuses** `/api/ws/exec`, `/api/pods`, `/api/namespaces` outright (403, JSON body). This listener is the node-port one, where nothing proves who is asking - so it never forwards these paths, and a client cannot smuggle the identity header through it |
| Traefik (`ingress.enabled`) | Routes those paths **straight to the API** (not through nginx) behind the `forwardauth-authelia` and `proxy-identity` middlewares. `forwardauth-authelia` declares `authResponseHeaders`, so Traefik *replaces* any `Remote-User` a client sent with Authelia's answer; `proxy-identity` adds the shared secret from the `naslos-internal-auth` Secret |
| `naslos-api` | Requires the proxy-issued shared secret **and** a non-empty identity header on every terminal endpoint, and fails closed: missing/absent/wrong secret → 401. The API refuses to start without `PROXY_SHARED_SECRET` unless `auth.disabled: true` is set (development only, logged loudly) |

The shared secret is what closes the forgery hole: an identity header alone proves
nothing when a client can reach the API directly (for example over the node port),
because anyone can send `Remote-User: admin`. Only the proxy knows the secret, and
the API never reads the identity header without it.

The API's exec permission is a namespaced `Role` (pods, pods/log, pods/exec) plus
a `ClusterRole` for namespace listing only, so it cannot exec into anything outside
Naslos. Shells are limited to `bash`/`sh`/`ash`/`zsh` - the endpoint runs a shell,
never an arbitrary command. Websocket upgrades are accepted only from the same
host (ports ignored). Sessions are logged with the authenticated user.

### Current state of this installation

There are **no `IngressRoute`/`Middleware` resources in the cluster** -
`ingress.enabled` is `false` in `values-vm.yaml`, so the chart's Traefik routes
(including the terminal's) are not rendered, and the node port is the only way to
reach the UI. The node port still refuses the three terminal paths in nginx, so
**the terminal is refused everywhere** - the intended behaviour for an
unauthenticated entry point.

Two ways forward:

```bash
# 1. Give the terminal its authenticated entry point (the intended shape):
#    deploys the chart's IngressRoutes + Authelia + proxy-identity middlewares
#    for <authelia.domain>, and keeps auth.disabled=false so the API requires
#    the proxy secret.
helm upgrade ... --set ingress.enabled=true

# 2. Or accept unauthenticated access on a trusted network (development only):
#    auth.disabled=true serves every owner API route without the proxy secret.
#    Note this does NOT open the terminal: nginx still refuses its paths on the
#    node port, so a terminal needs option 1.
helm upgrade ... --set auth.disabled=true
```

Option 1 is what the interactive Playwright test needs (reached through Traefik
with Authelia), which is why that test skips when the chart's ingress is not
deployed.

The terminal container itself exposes nothing: it runs no server, listens on no
port, and is reachable only through the API's exec endpoint. If the terminal is
not wanted at all, `terminal.enabled: false` removes the container and the exec
RBAC together.


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
