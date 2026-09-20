# Notifications (ntfy)

Naslos pushes alerts to [ntfy](https://ntfy.sh) topics. Notifications are
managed by `notifications.Manager` and configured under **Notifications** in the
UI (or via `GET/PUT /api/notifications`).

## Settings

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Master on/off switch |
| `serverUrl` | `https://ntfy.sh` | ntfy server URL; empty falls back to the public `https://ntfy.sh` (there is no bundled ntfy chart) |
| `topic` | `naslos-alerts` | Topic to publish to |
| `hasAuthToken` | `false` | **Read-only.** Whether a bearer token is stored; the token itself is never returned |
| `authToken` | — | **Write-only.** Omitted on save keeps the stored token; `""` clears it; any other value replaces it |
| `email` | — | `X-Email` header (server-side email sending) |
| `enabledEvents` | `[zfs_health, app_status, disk_failure]` | Which event types fire |
| `minSeverity` | `warning` | Only send at/above this severity |

## Event types

| Event | Meaning |
| --- | --- |
| `zfs_health` | Pool health changes / scrub events |
| `zfs_scrub` | Scrub start/stop |
| `app_status` | App install/upgrade/uninstall transitions |
| `disk_failure` | Disk health alarms |
| `system_update` | Talos/Naslos update availability |
| `share_access` | Share-level access events |
| `backup_success` | Buddy Backup run completed |
| `backup_failure` | Buddy Backup run failed |

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