# Notifications (ntfy)

Naslos pushes alerts to [ntfy](https://ntfy.sh) topics. Notifications are
managed by `notifications.Manager` and configured under **Notifications** in the
UI (or via `GET/PUT /api/notifications`).

## Settings

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Master on/off switch |
| `serverUrl` | `https://ntfy.sh` | ntfy server URL; empty falls back to the public `https://ntfy.sh` (there is no bundled ntfy chart). Only `http`/`https` are accepted, and link-local/metadata addresses are refused |
| `topic` | `naslos-alerts` | Topic to publish to (path-escaped when sent) |
| `hasAuthToken` | `false` | **Read-only.** Whether a bearer token is stored; the token itself is never returned |
| `authToken` | — | **Write-only.** Omitted on save keeps the stored token; `""` clears it; any other value replaces it |
| `email` | — | `X-Email` header (server-side email sending) |
| `enabledEvents` | `[zfs_health, disk_failure]` | Which event types fire |
| `minSeverity` | `warning` | Only send at/above this severity |

## Event types

Only the events below are implemented; the manager rejects nothing but the UI
offers exactly these, and the API only emits these.

| Event | Meaning |
| --- | --- |
| `zfs_health` | A pool's health leaves `ONLINE` (and once when it recovers). Polled every 5 minutes by the API health watcher |
| `disk_failure` | A disk that was present disappears from the node's inventory. Polled every 5 minutes |
| `backup_success` | Buddy Backup run completed |
| `backup_failure` | Buddy Backup run failed |

The watcher interval is `HEALTH_NOTIFY_INTERVAL_SECONDS` (default 300). A pool or
disk transition is reported once, not on every poll.

## Severity → priority mapping

| Severity | ntfy `X-Priority` |
| --- | --- |
| info | 2 |
| warning | 3 |
| error | 4 |
| critical | 5 |

## Delivery

`Manager.Send(Notification)` drops messages when:

- notifications are disabled, or
- severity is below `minSeverity`.

Otherwise it POSTs to `{serverUrl}/{topic}` with headers:

- `Title` — notification title
- `X-Priority` — mapped severity
- `Tags` — comma-separated tags
- `Authorization: Bearer <token>` — if configured
- `X-Email` — if configured

HTTP client timeout is 10 s.

## External server

There is **no bundled ntfy**: the ntfy subchart dependency was intentionally
dropped (see the comment in `charts/naslos/Chart.yaml`). Notifications go to an
external ntfy server:

| `serverUrl` | Behavior |
| --- | --- |
| empty | Falls back to `https://ntfy.sh` in code |
| `https://ntfy.sh` (or any URL) | Publish to that server / topic |

Default setting in code is `https://ntfy.sh` with topic `naslos-alerts`.

## API

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/notifications` | Current settings |
| PUT | `/api/notifications` | Update settings |
| POST | `/api/notifications/test` | Send a test notification ("Naslos Test", info severity) |

See [api.md](api.md) for the full route table.