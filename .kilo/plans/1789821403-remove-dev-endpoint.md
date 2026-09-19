# Remove the dev endpoint: authenticated-only

Goal (user decision: **full removal**, no break-glass): there must be no
unauthenticated listener and no authentication bypass anywhere. Delete the UI
NodePort, the chart's `auth.disabled`, and the `AUTH_DISABLED` /
`AGENT_AUTH_DISABLED` bypasses in the API and agent; make the production posture
the only posture; update the spec MUSTs and the tests that depend on the bypass.

Baseline: `master` `4cb330a` (PR #22 merged), live on helm revision 15 with
`naslos-api:0.1.0-r3`, `naslos-agent:0.1.0-r2`, `naslos-ui:0.1.0-r4`.

## Where the dev path lives today

| Surface | Location |
|---|---|
| NodePort service | `charts/naslos/templates/ui-nodeport.yaml`, `ui.nodePort` in `values.yaml` + `values-vm.yaml` |
| API bypass | `AUTH_DISABLED` read in `server.go:107`, `authDisabled` field (`:55`), passthrough in `requireOwnerAuth`/`requireAdmin` (`:302`,`:313`) |
| Agent bypass | `AGENT_AUTH_DISABLED` in `agent/cmd/main.go:64`, `authDisabled` in `agent/internal/server/server.go:40,67` |
| Chart gating | `naslos.authDisabled`/`naslos.authEnabled` in `_helpers.tpl`; `authEnabled` guards in `api-deployment.yaml:131`, `agent-daemonset.yaml:45`, `proxy-secret.yaml:1`, `agent-token.yaml:12`, `ingress.yaml:20`, `ingress-api.yaml:24` |
| Dev profile | `values-vm.yaml` (`auth.disabled: true`, `ui.nodePort.enabled: true`), `values-prod.yaml`, `Makefile install-prod` |
| nginx | terminal-path refusals in `ui/nginx.conf:51+`, written for the node-port listener |
| Tests | `routes_test.go` `newTestServer(t, authDisabled)`, `TestAuthDisabledPassthrough`, `notifications_test.go:27`, `buddy_send_test.go:224`; agent `auth_test.go:64`, `backup_test.go:73` |
| Spec/docs | `spec.md` MUSTs at 116/120/392 and the SEC-10 mapping row at 694; `deployment.md`, `identity-sso.md`, `CODE-REVIEW.md` (NAS-008, line 631), `AI_Handoff.md` |

## Changes

### Chart
1. Delete `templates/ui-nodeport.yaml`; drop the `ui.nodePort` block from
   `values.yaml` and `values-vm.yaml`.
2. `_helpers.tpl`: delete `naslos.authDisabled` and `naslos.authEnabled`.
3. Remove the `authEnabled` guards and keep the objects unconditional:
   `proxy-secret.yaml:1`, `agent-token.yaml:12`, the `proxy-identity` middleware
   entries in `ingress.yaml:20` / `ingress-api.yaml:24`, the `PROXY_SHARED_SECRET`
   block in `api-deployment.yaml:131`, the `AGENT_TOKEN` block in
   `agent-daemonset.yaml:45`.
4. `api-deployment.yaml`: delete the `AUTH_DISABLED` env (`:127`).
   `agent-daemonset.yaml`: delete the `AGENT_AUTH_DISABLED` env (`:43`).
5. `values.yaml`: delete `auth.disabled`; rewrite the `ingress.enabled` comment
   (`:375-383`) that tells operators to match `auth.disabled` and reach the UI
   over the NodePort; delete the `ui.nodePort` comment (`:89-93`).
6. `values-vm.yaml`: delete `auth.disabled: true` and `ui.nodePort`; add the
   production posture that lived in `values-prod.yaml` (`ingress.enabled: true`,
   `authelia.domain: naslos.local`, `traefik.ports.web.hostPort: 80`,
   `websecure.hostPort: 443`, `traefik.service.type: ClusterIP`).
7. Delete `values-prod.yaml`; `Makefile`: drop `install-prod` and its `.PHONY`
   entry. `deploy-vm.sh` keeps calling `make install-vm`, which now installs the
   authenticated posture.

### API
8. `server.go`: delete the `authDisabled` field (`:52-55`), the `AUTH_DISABLED`
   read and warning (`:104-120`), and `authDisabled: authDisabled` (`:171`);
   make the check unconditional —
   `if proxySecret == "" { log.Fatalf("PROXY_SHARED_SECRET is required; set it from the naslos-proxy Secret") }`;
   `requireOwnerAuth`/`requireAdmin` always wrap (`:302-318`); fix the comments at
   `:200` and `:291`.
9. Drop the AUTH_DISABLED remarks at `websocket.go:317` and `pods.go:61`.
10. Tests:
    - `routes_test.go`: `newTestServer(t)` (no bool; always `PROXY_SHARED_SECRET`
      + the synthetic CIDR), update the four call sites, delete
      `TestAuthDisabledPassthrough`. Keep `TestOwnerRoutesRequireAuth` as the
      regression test — it is stronger now that no opt-out exists.
    - `notifications_test.go:27`: `newTestServer(t)`; requests already carry the
      secret where needed.
    - `buddy_send_test.go`: the harness builds a bare `Server`, so give it a real
      middleware (`auth.NewMiddleware([]string{"192.0.2.0/24"}, "test-secret")`)
      and have `call()` set `Remote-User: admin`, `Remote-Groups: naslos_admins`
      and `X-Naslos-Proxy-Secret: test-secret`. Update the comments at `:219` and
      `:370`.

### Agent
11. `cmd/main.go`: delete `AGENT_AUTH_DISABLED`; require `AGENT_TOKEN`
    unconditionally (`log.Fatalf` if empty).
12. `internal/server/server.go`: delete the `authDisabled` field/opt-out and the
    `AuthDisabled` option; always require the bearer token. Update the doc
    comment at `:26`.
13. Tests: `auth_test.go` drop the disabled case (`:64-69`); `backup_test.go:73`
    construct with `authToken: "test-token"` and send the header.

### UI and E2E
14. `ui/nginx.conf`: remove the terminal-path refusal block (`:51+`). It existed
    to close the node-port listener; the API's secret+CIRR gate is the control,
    and the ingress routes `/api` straight to the API. Keep the `/api` proxy for
    in-cluster/legacy parity.
15. `playwright.config.ts`: default baseURL `https://naslos.local` (the `.96`
    default is retired).
16. `auth.setup.ts`: remove the "no portal → write empty state" dev branch;
    require the portal and fail fast with a clear message if the target is not
    proxied. `terminal.spec.ts`: drop the NodePort wording (keep accepting
    302/401/403 for the anonymous case). `backups.spec.ts`: drop the "or nothing
    on the dev posture" comment.

### Spec and docs
17. `spec.md`: the MUSTs that require an explicit, default-off, logged opt-out
    (`:116`, `:120`, `:392`) become fail-closed-only MUSTs; the SEC-10 mapping row
    (`:694`) drops the "with the dev opt-out → served" half and points at
    `TestOwnerRoutesRequireAuth` / `TestOwnerRoutesPassWithTheProxySecret`.
18. `deployment.md`: remove the dev posture, `install-prod`, `values-prod`, the
    "rollback to values-vm" instructions and the NodePort references; the E2E
    section drops the dev command.
19. `identity-sso.md:161`: point at `values-vm.yaml` for `hostPort`.
20. `CODE-REVIEW.md`: NAS-008 → resolved (listener removed); update the
    `auth.disabled` notes at `:524`/`:631`.
21. `AI_Handoff.md`: remove the dev-posture/break-glass text; rollback is now
    `git revert` + redeploy, or `helm rollback`.

## Rollout

1. Bump and push images: api `0.1.0-r4`, agent `0.1.0-r3`, ui `0.1.0-r5`; update
   the tags in `values-vm.yaml`.
2. `make install-vm` (now the authenticated posture). API and agent must roll in
   the same upgrade: the token/secret are always rendered, so an old agent and a
   new API stay compatible, but a partially rolled state is expected mid-upgrade.
3. Confirm the NodePort Service is gone.

## Verification

- `cd api && go build ./... && go vet ./... && go test ./...`
- `cd agent && go build ./... && go vet ./... && go test ./...`
- `cd ui && npm run check`
- `helm lint` + `helm template` for `values.yaml` and `values-vm.yaml`:
  no `naslos-ui-nodeport`, no `AUTH_DISABLED`/`AGENT_AUTH_DISABLED`, no
  `ui.nodePort`, the proxy/agent Secrets always present, `proxy-identity` on
  every route.
- `grep -rn "AUTH_DISABLED\|AGENT_AUTH_DISABLED\|auth.disabled\|nodePort"` over
  the tree returns only historical `.kilo/plans/` and `docs/archive/` files.
- Live on .117: `kubectl -n naslos get svc` has no NodePort; `curl http://192.168.1.117:30080`
  refuses; anonymous `https://naslos.local/api/users` → 302; `kubectl -n naslos
  exec` into a pod without the secret → 401; the E2E suite 35 passed.

## Risks

- **No break-glass.** If Authelia/Traefik misbehaves, recovery is cluster access
  (`helm rollback` / `git revert`), not an env var. Accepted with the decision.
- **Generic installs:** `values.yaml` keeps `ingress.enabled: false`, so a chart
  installed without the VM profile now has no listener at all (previously it fell
  back to the NodePort). The VM profile is the reference; note this in
  `values.yaml` so it is a deliberate choice, not a surprise.
- **Spec MUSTs change:** dropping the opt-out MUSTs is a spec change and must land
  with the updated tests in the same commit (VER-4).
- **Partial rollbacks:** api and agent both now demand their credential; the
  chart always provides it, so `helm rollback` to the old revision still works
  (old revision carried `AUTH_DISABLED=false` on the VM's prod posture).

## Out of scope

- Editing historical plans/archive logs that mention the dev posture.
- `auth.proxySecretName` / `agent.tokenSecret` overrides: those supply a secret,
  they are not a bypass, and stay.
- The Traefik dashboard link, `NASLOS_RECEIVER_URL`, TOTP enrolment.
