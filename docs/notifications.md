# Notifications (ntfy + webhook)

Naslos pushes alerts to two channels: [ntfy](https://ntfy.sh) topics and a
generic HTTP webhook. The ntfy channel is managed by `notifications.Manager`
and the webhook channel by `webhook.Manager`; both are configured under
**Notifications** in the UI. Each channel has its own persistence file, event
routing and severity floor, so disabling one does not affect the other.

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
| `zfs_health` | A pool's health leaves `ONLINE`, a disk's SMART temperature crosses `DISK_HEALTH_TEMPERATURE_C` (default 55 °C), its wear reaches `DISK_HEALTH_WEAR_PERCENT` (default 80 %), or a vdev is `DEGRADED`; per-disk conditions send one recovery notice when they clear. Polled every 60 s |
| `disk_failure` | SMART reports `FAILED`, or a ZFS vdev is `FAULTED`/`OFFLINE`/`UNAVAIL`/`REMOVED`; one recovery notice when it clears. Polled every 60 s. See [operations.md](operations.md#disk-health) |
| `backup_success` | Buddy Backup run completed |
| `backup_failure` | Buddy Backup run failed |

The watcher interval is `HEALTH_INTERVAL_SECONDS` (default 60). A pool, disk or
threshold transition is reported once, not on every poll. Per-disk conditions
send one recovery notice; a pool returning to `ONLINE` is cleared silently.
`DISK_HEALTH_ENABLED=false` disables the per-disk alerts while leaving the pool
watcher and the Disk Health page working.

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

## Webhook channel

A generic JSON webhook is the second alert sink (FR-ALT). It is configured
independently of ntfy: its own target URL, optional bearer token, routed
events and minimum severity. Settings live in `ALERT_WEBHOOK_CONFIG`
(default `/var/lib/naslos/webhook.json`, mode `0600` because it may hold the
token), set by the chart value `api.webhookFile`.

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Channel on/off switch |
| `url` | — | Target `http`/`https` URL. Required when enabled. Validated at save **and** at send time |
| `hasAuthToken` | `false` | **Read-only.** Whether a bearer token is stored; the token itself is never returned |
| `authToken` | — | **Write-only.** Omitted on save keeps the stored token; `""` clears it; any other value replaces it |
| `enabledEvents` | `[zfs_health, disk_failure]` | Which event types this channel receives |
| `minSeverity` | `warning` | Only send at/above this severity |

The event names are the same ones the ntfy channel uses (see above), plus the
manual `test` action. Routing is per channel: an event can go to ntfy, the
webhook, both, or neither.

### URL and SSRF policy

The URL guard is the shared one used by ntfy (`blocked_ip`):

- only `http` and `https` are accepted;
- embedded credentials (`https://user:pass@host/…`) are refused;
- loopback, private, link-local, CGNAT, benchmark, reserved, metadata and ULA
  addresses are refused, including IPv4-mapped IPv6 forms;
- a hostname is resolved at save time and again at send time, and the send is
  pinned to the vetted address so DNS cannot be rebound between the check and
  the request;
- redirects are disabled.

There is no private-target allowlist, so a receiver on a LAN address is not
reachable by this channel.

### Payload

The webhook receives `POST` with `Content-Type: application/json`,
`User-Agent: naslos-alerts`, and `Authorization: Bearer <token>` only when a
token is configured. The body is bounded to the event data:

```json
{
  "source": "naslos",
  "event": "disk_failure",
  "title": "Disk sda SMART health FAILED",
  "message": "SMART reports disk sda as failing. Replace it and resilver the pool.",
  "severity": "error",
  "tags": ["floppy_disk"]
}
```

No channel settings (`url`, token) are part of the payload.

### Delivery semantics

- One attempt per event. No retries and no queue: an unreachable receiver is
  logged and the next event starts fresh.
- HTTP timeout is 10 s and redirects are not followed.
- A response status `>= 400` is a failed delivery. Response bodies are drained
  to a fixed cap (8 KiB) and discarded; they are never logged or returned, so a
  receiver cannot reflect a request header (or token) back through the API.
- Transport errors and non-2xx statuses are reported by kind/status only, never
  with the URL, path or query string.
- Each channel is attempted independently; a failure in one is logged without
  suppressing the other.

### API

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/notifications/webhook` | Current webhook settings (never the token) |
| PUT | `/api/notifications/webhook` | Update settings (`400` for a bad URL/severity) |
| DELETE | `/api/notifications/webhook` | Reset the channel to defaults (disabled, no URL/token) |
| POST | `/api/notifications/webhook/test` | Send a test payload (`400` when disabled, `502` on delivery failure) |

## API

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/notifications` | Current ntfy settings |
| PUT | `/api/notifications` | Update ntfy settings |
| POST | `/api/notifications/test` | Send a test notification ("Naslos Test", info severity) |
| GET/PUT/DELETE | `/api/notifications/webhook` | Generic webhook channel settings / reset |
| POST | `/api/notifications/webhook/test` | Send a test webhook |

See [api.md](api.md) for the full route table.