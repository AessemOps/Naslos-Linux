# MCP server (AI assistant access)

Naslos ships a first-party MCP (Model Context Protocol) endpoint so an AI
assistant — or any MCP client — can manage the NAS with typed tools instead
of scraping the web UI. It is a Streamable HTTP server **inside the existing
`naslos-api` pod**: no new image, Service or NetworkPolicy, and it inherits
the API's TLS, validation and error shapes.

- Spec: `docs/spec.md` §3.14 (`FR-MCP`) and `SEC-17`
- Route: `https://<your-naslos-domain>/mcp` (same host as the web UI)
- Image: none new — the `naslos-api` image, guarded by its own middleware

## Security model

The endpoint is **off by default** and answers `404` while disabled, so an
off-by-default install does not advertise it. `mcp.available: false` in the
chart is a stronger, install-time kill switch: it removes the Traefik route
and the Authelia bypass entirely.

Authentication is **not** Authelia (a programmatic client cannot complete an
interactive 2FA login). It is:

1. The same trust gate every `/api` request passes (SEC-10 has no opt-out):
   the request must come through Traefik and carry the proxy secret.
2. An `Authorization: Bearer nmcp_…` token created by an admin in Settings.
   Only the SHA-256 hash is stored; the plaintext is shown exactly once at
   creation, and comparison is constant-time.
3. An Origin check: a present `Origin` must be the NAS's own domain, so a
   browser page cannot drive the endpoint cross-site (MCP DNS-rebinding
   guidance). Non-browser clients send no `Origin` and pass.
4. Failure backoff: ten failed attempts from one source address inside five
   minutes block that address for five minutes.

### Scopes

Every tool declares the minimum scope it needs, and every token carries one
scope:

| Scope | Reads | Changes | Destructive |
| --- | --- | --- | --- |
| `read` | yes | no | no |
| `write` | yes | yes (create a share, install an app, start a VM, …) | no |
| `full` | yes | yes | yes (destroy a pool, uninstall an app, reboot, …) |

`tools/list` hides tools above the token's scope, and `tools/call` re-checks
(a client can call an unlisted tool). A **`full` token is an administrator
without two-factor authentication**: default new tokens to `read`, prefer an
expiry for anything stronger, and revoke tokens you no longer use.

### The `confirm` rule

Destructive tools (`full` scope, `destructive: true` annotation) require one
extra argument:

```json
{ "name": "old-media", "confirm": "old-media" }
```

`confirm` must equal the **exact target** of the action — the same resolved
value used in the call (the pool name, app name, version, or the literal
`reboot`). The server checks it before dispatching anything, so a wrong or
missing `confirm` never reaches the API. This is the guard that keeps an
enthusiastic LLM from destroying "old-media" when the user asked about it.

## Connect a client

Create a token in **Settings → MCP** (the value is shown once), then:

**Streamable HTTP clients** (Claude Desktop, Cursor, Kilo, …):

```json
{
  "mcpServers": {
    "naslos": {
      "type": "http",
      "url": "https://naslos.example/mcp",
      "headers": { "Authorization": "Bearer nmcp_YOUR_TOKEN" }
    }
  }
}
```

**stdio-only clients** bridge through
[mcp-remote](https://github.com/geelen/mcp-remote):

```sh
npx mcp-remote https://naslos.example/mcp --header "Authorization: Bearer nmcp_YOUR_TOKEN"
```

Notes:

- **TLS**: the client must trust the NAS domain's certificate. A public ACME
  certificate just works; a private CA must be installed on the client
  machine.
- **Stateless**: the server is a plain request/response MCP endpoint (JSON
  preferred, SSE only when a handler streams). There are no sessions to
  resume.
- **Capabilities**: `tools` only — no resources or prompts in v1.

## The tool catalog

The catalog mirrors the owner API surface (about 128 tools): system
(dashboard, metrics, versions, upgrade, reboot), disks, pools, datasets,
shares, users and groups, apps (catalog, install, jobs, exposure, sources),
domains/DNS/DDNS, network changes, notifications, VMs, Buddy backup, pods
and a bounded `pod_logs` snapshot (the last N lines — the live log stream
stays in the terminal). Long-running operations return a job or operation
id; poll the matching `*_get` tool instead of repeating the call.

The mapping is a declarative registry in `rust/api/src/mcp/tools/` — one
file per domain — dispatched through the same router the web UI uses, so
tool results are exactly the API's JSON responses. Nothing in the tool
layer re-implements domain logic.

## What is logged

Every `tools/call` produces a durable record in the append-only audit log
(`docs/rbac-audit.md` §7: actor `mcp`, token id/name, scope, tool, generated
MCP call id, HTTP status/refusal code, duration) **and** the existing
structured log line plus Settings-visible recent calls feed. Refused calls
(unknown tool, scope, missing `confirm`, bad path) are recorded with
`outcome: denied|failed` before the error is returned. **Arguments are never
logged or recorded** — tool bodies can carry passwords and secrets. A mutating tool
(`write`/`full` scope) is not dispatched unless its begin record is durable: a
degraded or unwritable audit sink refuses the call, exactly like an unsafe
HTTP route. Enabling/disabling the endpoint and token creation/revocation
happen through the owner API, which is to say: with a 2FA-authenticated admin
session.
