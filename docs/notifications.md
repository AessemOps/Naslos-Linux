# Notifications (ntfy, webhook, SMTP, syslog)

Naslos pushes alerts to four channels: [ntfy](https://ntfy.sh) topics, a
generic HTTP webhook, SMTP email and syslog. The ntfy channel is managed by
`notifications.Manager`, the webhook channel by `webhook.Manager`, and the SMTP
and syslog channels by `smtp.Manager` and `syslog.Manager`; all are configured
under **Notifications** in the UI. Each channel has its own persistence file,
event routing and severity floor, so disabling one does not affect the others.
Every selected event is delivered to every channel concurrently and
independently: a timeout or failure in one channel cannot suppress or delay
another.

## Settings

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Master on/off switch |
| `serverUrl` | `https://ntfy.sh` | **Write-only.** ntfy server URL; required when enabled (there is no bundled ntfy chart). Only `http`/`https` are accepted; link-local/metadata addresses are refused. An omitted field keeps the stored value, `""` clears it |
| `topic` | `naslos-alerts` | **Write-only.** Topic to publish to; required when enabled and restricted to letters, digits, `-` and `_` |
| `hasServerUrl` / `hasTopic` | — | **Read-only.** Whether a destination is stored; the values are never returned |
| `hasAuthToken` | `false` | **Read-only.** Whether a bearer token is stored; the token itself is never returned |
| `hasUrlToken` | — | **Read-only.** Whether a destination and/or bearer token is configured |
| `hasEmail` | — | **Read-only.** Whether an `X-Email` address is stored |
| `authToken` | — | **Write-only.** Omitted on save keeps the stored token; `""` clears it; any other value replaces it |
| `email` | — | **Write-only.** `X-Email` header (server-side email sending) |
| `allowInsecureHttp` | `false` | Explicit acknowledgement for a bearer token over plaintext `http`; audited as `notifications` / `insecure_http_acknowledged` |
| `enabledEvents` | `[zfs_health, disk_failure]` | Which event types fire |
| `minSeverity` | `warning` | Only send at/above this severity |

`GET /api/notifications` returns only `category: "ntfy"`, the presence flags
above, `enabled`, `allowInsecureHttp`, `enabledEvents` and `minSeverity` — never
the server URL, topic, email or token. `PUT /api/notifications` returns `400`
for an unreachable configuration at save time (enabled with no server URL or
topic, an invalid URL/topic/severity, or an unacknowledged token over `http`).

## Event types

Only the events below are implemented; the manager rejects nothing but the UI
offers exactly these, and the API only emits these.

| Event | Meaning |
| --- | --- |
| `zfs_health` | A pool's health leaves `ONLINE`, a disk's SMART temperature crosses `DISK_HEALTH_TEMPERATURE_C` (default 55 °C), its wear reaches `DISK_HEALTH_WEAR_PERCENT` (default 80 %), or a vdev is `DEGRADED`; per-disk conditions send one recovery notice when they clear. Polled every 60 s |
| `disk_failure` | SMART reports `FAILED`, or a ZFS vdev is `FAULTED`/`OFFLINE`/`UNAVAIL`/`REMOVED`; one recovery notice when it clears. Polled every 60 s. See [operations.md](operations.md#disk-health) |
| `backup_success` | Buddy or cloud backup run completed (cloud sends carry the `cloud` tag) |
| `backup_failure` | Buddy or cloud backup run failed (cloud sends carry the `cloud` tag) |
| `audit` | The append-only audit log entered the degraded state; mutating API requests are refused until an operator repairs it. Also sent directly to ntfy as critical |

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

A dropped message is not an error. The manual `POST /api/notifications/test`
action is different: it requires the channel to be enabled (`400` when it is
not) and publishes the info-severity test message directly, bypassing event
routing and the severity floor, so the transport is verified even when
`minSeverity` is `warning` or higher. A delivery failure is `502`.

HTTP client timeout is 10 s. Redirects are disabled, so a 3xx response is a
failed delivery, not a success. Delivery errors are category-only (timeout,
connect, redirect, rejected status); they never contain the publish URL, the
topic or the token.

## External server

There is **no bundled ntfy**: the ntfy subchart dependency was intentionally
dropped (see the comment in `charts/naslos/Chart.yaml`). Notifications go to an
external ntfy server:

| `serverUrl` | Behavior |
| --- | --- |
| empty | Accepted only while the channel is disabled; enabling it requires a URL |
| `https://ntfy.sh` (or any URL) | Publish to that server / topic |

Default setting in code is `https://ntfy.sh` with topic `naslos-alerts`. A
bearer token over plaintext `http` is refused unless `allowInsecureHttp` is
explicitly set; the acknowledgement is written to the audit log and every send
emits a warning.

## Webhook channel

A generic JSON webhook is the second alert sink (FR-ALT). It is configured
independently of ntfy: its own target URL, optional bearer token, routed
events and minimum severity. Settings live in `ALERT_WEBHOOK_CONFIG`
(default `/var/lib/naslos/webhook.json`, mode `0600` because it may hold the
token), set by the chart value `api.webhookFile`.

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Channel on/off switch |
| `url` | — | **Write-only.** Target `http`/`https` URL; required when enabled. Syntax-checked at save and resolved at send time |
| `hasUrl` / `hasUrlToken` | — | **Read-only.** Whether a URL and/or bearer token is configured; the values are never returned |
| `hasAuthToken` | `false` | **Read-only.** Whether a bearer token is stored; the token itself is never returned |
| `allowInsecureHttp` | `false` | Explicit acknowledgement for a bearer token over plaintext `http`; audited as `notifications` / `insecure_http_acknowledged` |
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
- a literal address is checked at save time; a hostname is resolved
  asynchronously at send time with `tokio::net::lookup_host`, and the send is
  pinned to the vetted address so DNS cannot be rebound between the check and
  the request;
- redirects are disabled, so a 3xx is a failed delivery;
- a bearer token over plaintext `http` requires `allowInsecureHttp: true`
  (audited, warned at send time).

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
| GET | `/api/notifications/webhook` | Redacted settings (`category`, presence flags only) |
| PUT | `/api/notifications/webhook` | Update settings (`400` for a bad URL/severity or unacknowledged token over `http`) |
| DELETE | `/api/notifications/webhook` | Reset the channel to defaults (disabled, no URL/token) |
| POST | `/api/notifications/webhook/test` | Send a test payload (`400` when disabled, `502` on delivery failure) |

## SMTP channel

The email channel (FR-ALT-07) keeps its own target, routing and severity
floor. Settings live in `ALERT_SMTP_CONFIG` (default
`/var/lib/naslos/smtp.json`, mode `0600` because it may hold the password), set
by the chart value `api.smtpFile`.

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Channel on/off switch |
| `host` | — | Relay hostname or IP. Required when enabled. No embedded `user:pass@`, path or `host:port` |
| `port` | `587` | Relay port (1–65535). `0` in a PUT selects the TLS-mode default |
| `tls` | `starttls` | `starttls`, `implicit` (SMTPS) or `none` |
| `username` | — | Optional SMTP AUTH username |
| `hasPassword` | `false` | **Read-only.** Whether a password is stored; the password itself is never returned |
| `password` | — | **Write-only.** Omitted on save keeps the stored password; `""` clears it; any other value replaces it |
| `from` | — | Envelope/header sender; a bare mailbox (`naslos@example.com`). Required when enabled |
| `to` | `[]` | Recipients; bare mailboxes. At most 20. Required when enabled |
| `enabledEvents` | `[zfs_health, disk_failure]` | Which event types this channel receives |
| `minSeverity` | `warning` | Only send at/above this severity |

The event names are the same ones ntfy and the webhook use, plus the manual
`test` action. Routing is per channel: an event can go to any subset of the
four channels.

### TLS and credentials

- `starttls` upgrades after `EHLO` and is refused if the server does not
  advertise `STARTTLS`.
- `implicit` starts TLS immediately (SMTPS, typically port 465).
- `none` is plaintext for an isolated LAN relay.
- **Certificate validation is always on.** The client trusts the compiled-in
  Mozilla CA roots and validates the certificate for the configured hostname.
  There is no insecure/accept-any option; a private relay that wants
  `starttls`/`implicit` must present a publicly trusted certificate, otherwise
  use `none` on a trusted network.
- Credentials are optional. A password requires a username, and AUTH is used
  only when both are set (a username whose password was cleared means "no
  authentication"). A password is refused with `tls = none`, so credentials
  never cross the network in clear text.
- `AUTH PLAIN` is preferred when advertised, otherwise `AUTH LOGIN`. A server
  that offers neither is a delivery error.

### Target policy

SMTP relays commonly live on the operator's LAN, so this channel uses the
LAN-aware guard instead of the strict webhook guard:

- an explicitly configured **RFC1918 IPv4** target (`10/8`, `172.16/12`,
  `192.168/16`) or a hostname that resolves only to those addresses is
  allowed;
- loopback, link-local (including `169.254.169.254` metadata), CGNAT,
  multicast, unspecified, documentation, benchmark, reserved and ULA
  addresses are refused, including IPv4-mapped IPv6 forms;
- a hostname is resolved when the settings are saved **and** again at send
  time, and the connection is pinned to the vetted address, so DNS cannot be
  rebound between the check and the send;
- the host field cannot carry URL credentials, a path or a second target.

The chart's network policy still has to allow the egress: use the configured
`networkPolicy.extraEgressCIDRs` (or a `nodeCIDR` that already covers the LAN)
for a relay outside the cluster.

### Message and delivery

The message is RFC 5322 plain text with CRLF endings and UTF-8:

```
From: naslos@example.com
To: ops@example.com
Subject: [Naslos] Disk sda is at 60°C
X-Naslos-Event: disk_failure
Date: Sat, 22 Nov 2026 11:00:00 +0000
MIME-Version: 1.0
Content-Type: text/plain; charset=utf-8
Content-Transfer-Encoding: 8bit

Disk /dev/sda is above the 55°C warning threshold.
```

- The subject and event header are stripped of CR/LF and control characters,
  so a notification title cannot inject a header. Sender and recipients are
  validated as bare mailboxes.
- DATA is dot-stuffed; the payload is bounded to 64 KiB.
- Delivery is one attempt under a 15 s whole-operation timeout. There is no
  retry queue and no unbounded buffering.
- A failure reports a fixed category and the SMTP numeric code only (for
  example `SMTP authentication failed (SMTP code 535)`); the host, username,
  password, server text and message body never appear in an error or log line.

## Syslog channel

Syslog forwarding (FR-ALT-09) sends one RFC 5424 message per selected event
over UDP, TCP or TLS. It has no credentials. Settings live in
`ALERT_SYSLOG_CONFIG` (default `/var/lib/naslos/syslog.json`), set by the chart
value `api.syslogFile`.

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Channel on/off switch |
| `protocol` | `udp` | `udp`, `tcp` or `tls` |
| `host` | — | Collector hostname or IP. Required when enabled |
| `port` | `514` | Collector port. An omitted/`0` port defaults to 514 for `udp`/`tcp` and 6514 for `tls` |
| `facility` | `local0` | Facility name (`kern`, `user`, `mail`, `daemon`, `auth`, `syslog`, `lpr`, `news`, `uucp`, `cron`, `authpriv`, `ftp`, `ntp`, `audit`, `console`, `clock`, `local0`–`local7`) or a number 0–23 |
| `enabledEvents` | `[zfs_health, disk_failure]` | Which event types this channel receives |
| `minSeverity` | `warning` | Only send at/above this severity |

### Payload

RFC 5424, one message per event:

```
<134>1 2026-11-22T11:00:00Z naslos naslos - disk_failure - Disk sda is at 60°C: Disk /dev/sda is above the 55°C warning threshold.
```

`PRI` is `facility × 8 + severity`. Naslos severities map to RFC 5424
severities:

| Severity | RFC 5424 code |
| --- | --- |
| info | 6 (informational) |
| warning | 4 (warning) |
| error | 3 (error) |
| critical | 2 (critical) |

- `HOSTNAME` and `APP-NAME` are `naslos`; `PROCID` and `STRUCTURED-DATA` are
  the RFC 5424 nil value `-`; `MSGID` is the sanitized event name.
- CR/LF/tab and other control characters in the title/message are replaced
  with spaces. The whole message is bounded to 8 KiB on a UTF-8 boundary.
- UDP sends one datagram; TCP and TLS use one newline-terminated frame and a
  connection that is closed after the write.
- TLS validates the collector certificate against the compiled-in Mozilla CA
  roots (no insecure mode).
- The target policy is the SMTP/LAN-aware one above: explicit RFC1918 targets
  are allowed; loopback, link-local/metadata, CGNAT, ULA and other
  non-routable addresses are refused.
- Delivery is one attempt under a 10 s whole-operation timeout. Errors name
  the failure category only, never the host or the payload.

## API

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/notifications` | Redacted ntfy settings (destination/token never returned) |
| PUT | `/api/notifications` | Update ntfy settings (`400` for an invalid or incomplete target) |
| POST | `/api/notifications/test` | Send a test notification ("Naslos Test", info severity; bypasses the severity floor; `400` when disabled, `502` on delivery failure) |
| GET/PUT/DELETE | `/api/notifications/webhook` | Generic webhook channel settings / reset |
| POST | `/api/notifications/webhook/test` | Send a test webhook |
| GET/PUT/DELETE | `/api/notifications/smtp` | SMTP channel settings / reset (never returns the password) |
| POST | `/api/notifications/smtp/test` | Send a test email |
| GET/PUT/DELETE | `/api/notifications/syslog` | Syslog channel settings / reset |
| POST | `/api/notifications/syslog/test` | Send a test RFC 5424 message |

See [api.md](api.md) for the full route table.