# Notifications (ntfy)

NasOS pushes alerts to [ntfy](https://ntfy.sh) topics. Notifications are
managed by `notifications.Manager` and configured under **Notifications** in the
UI (or via `GET/PUT /api/notifications`).

## Settings

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Master on/off switch |
| `serverUrl` | `https://ntfy.sh` | ntfy server; empty = bundled ntfy (`values.yaml: ntfy.server.url`) |
| `topic` | `nasos-alerts` | Topic to publish to |
| `authToken` | — | Bearer token when the server requires auth |
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
| `system_update` | Talos/NasOS update availability |
| `share_access` | Share-level access events |

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

## Bundle vs external

`values.yaml: ntfy.server.url` decides the topology:

| Value | Behavior |
| --- | --- |
| empty | Use the bundled `ntfy` Helm chart (same cluster) |
| `https://ntfy.sh` (or any URL) | Publish to an external server / topic |

Default setting in code is `https://ntfy.sh` with topic `nasos-alerts`.

## API

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/notifications` | Current settings |
| PUT | `/api/notifications` | Update settings |
| POST | `/api/notifications/test` | Send a test notification ("NasOS Test", info severity) |

See [api.md](api.md) for the full route table.