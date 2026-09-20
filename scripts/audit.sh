#!/bin/sh
# Naslos audit sweep (see docs/AUDIT-2026-09-19-REPORT.md).
#
# Reproducible checks that are fast enough for a pre-PR run. Missing tools are
# reported as "skipped" rather than failing, so it runs anywhere. There is no CI
# workflow wired to it for now - run it by hand, or add a workflow later.
#
# Usage:  sh scripts/audit.sh
#         NASLOS_AUDIT_RACE=1 sh scripts/audit.sh   # adds go test -race (CR-06)
set -u

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
fails=0

run() {
  name=$1
  shift
  printf '\n== %s ==\n' "$name"
  if "$@"; then
    : # ok
  else
    fails=$((fails + 1))
    printf 'FAILED: %s\n' "$name"
  fi
}

skip() {
  printf '\n== %s ==\nskipped: %s\n' "$1" "$2"
}

have() { command -v "$1" >/dev/null 2>&1; }

# AUDIT-M3 regression guard: the LDAP bind password must reach Authelia as an
# env-pointed mounted Secret file, never inline in the authelia-config ConfigMap.
# This is the assertion the revision-38 attempt lacked: it would have failed on
# the missing AUTHELIA_AUTHENTICATION_BACKEND_LDAP_PASSWORD_FILE env var and on
# the additionalSecrets mount path not matching the env path.
check_authelia_ldap_secret() {
  f=$(mktemp) || return 1
  if ! helm template naslos "$root/charts/naslos" -n naslos \
    -f "$root/charts/naslos/values.yaml" -f "$root/charts/naslos/values-vm.yaml" \
    --set openldap.bindPassword=lint-only >"$f" 2>/dev/null; then
    rm -f "$f"
    return 1
  fi
  ok=1
  grep -q 'name: AUTHELIA_AUTHENTICATION_BACKEND_LDAP_PASSWORD_FILE' "$f" || ok=0
  grep -q "value: '/secrets/naslos-openldap/service-password'" "$f" || ok=0
  grep -q 'mountPath: /secrets/naslos-openldap' "$f" || ok=0
  # No inline bind password in the authelia-config ConfigMap. Anchor on the
  # ConfigMap's metadata name (two-space indent): the DaemonSet also has a
  # `name: authelia-config` volume reference at a deeper indent.
  if sed -n '/^  name: authelia-config$/,/^---$/p' "$f" | grep -qE '^[[:space:]]+password:'; then
    ok=0
  fi
  rm -f "$f"
  [ "$ok" -eq 1 ]
}

# AUDIT-M4 guard: the namespace must render the default-deny and the per-workload
# allow policies, and the agent port must be open to the API as its ONLY pod
# peer. Inert under flannel, but the intent must not silently disappear or be
# widened before a policy CNI lands. Parses the rendered manifests with python
# (a plain grep passes if the policy is inverted or a permissive peer is added).
check_network_policies() {
  f=$(mktemp) || return 1
  if ! helm template naslos "$root/charts/naslos" -n naslos \
    -f "$root/charts/naslos/values.yaml" -f "$root/charts/naslos/values-vm.yaml" \
    --set openldap.bindPassword=lint-only >"$f" 2>/dev/null; then
    rm -f "$f"
    return 1
  fi
  python3 - "$f" <<'PY'
import sys, yaml
docs = [d for d in yaml.safe_load_all(open(sys.argv[1])) if d]
nps = {d["metadata"]["name"]: d for d in docs if d.get("kind") == "NetworkPolicy"}
required = ["naslos-default-deny", "naslos-agent-ingress", "naslos-workload-egress"]
for name in required:
    assert name in nps, f"missing policy {name}"

agent = nps["naslos-agent-ingress"]
assert agent["spec"]["podSelector"]["matchLabels"] == {
    "app.kubernetes.io/name": "naslos-agent"
}, "agent policy selects the wrong pods"

ingress = agent["spec"]["ingress"]
assert len(ingress) == 1, "agent policy must have exactly one ingress rule"
rule = ingress[0]
peers = rule.get("from", [])
assert len(peers) == 1, f"agent policy must have exactly one peer, got {peers!r}"
assert peers[0].get("podSelector", {}).get("matchLabels") == {
    "app.kubernetes.io/name": "naslos-api"
}, f"agent policy peer is not the API alone: {peers!r}"
assert {p.get("port") for p in rule.get("ports", [])} == {9090}, "agent policy must allow only :9090"

# No ingress ipBlock on the agent: any CIDR there would reopen it broadly.
assert all("ipBlock" not in p for p in peers), "agent policy must not allow a CIDR peer"

# The pod CIDR must never appear as an ingress source (it would admit every pod).
for name, d in nps.items():
    for r in d["spec"].get("ingress", []):
        for p in r.get("from", []):
            cidr = p.get("ipBlock", {}).get("cidr", "")
            assert cidr != "10.244.0.0/16", f"{name} admits the whole pod CIDR"
print("network policy intent ok")
PY
  rc=$?
  rm -f "$f"
  [ "$rc" -eq 0 ]
}

# --- Go ---------------------------------------------------------------------
cd "$root/api" || exit 1
run "go vet (api)" go vet ./...
run "go test (api)" go test ./...
# CR-06 fixed: the buddy scheduler's notification race is gone, so the race
# detector runs unconditionally.
run "go test -race (api)" go test -race ./...
if have govulncheck; then
  run "govulncheck (api)" govulncheck ./...
else
  skip "govulncheck (api)" "not installed (go install golang.org/x/vuln/cmd/govulncheck@latest)"
fi
if have gosec; then
  run "gosec high severity (api)" gosec -quiet -severity high ./...
else
  skip "gosec (api)" "not installed (go install github.com/securego/gosec/v2/cmd/gosec@latest)"
fi

cd "$root/agent" || exit 1
run "go vet (agent)" go vet ./...
run "go test -race (agent)" go test -race ./...

# --- UI ---------------------------------------------------------------------
cd "$root/ui" || exit 1
if have npm; then
  if [ -f package-lock.json ]; then
    run "npm audit (prod)" npm audit --omit=dev
  else
    skip "npm audit" "no package-lock.json"
  fi
  if [ -d node_modules ]; then
    run "svelte-check" npm run check --silent
  else
    skip "svelte-check" "run npm ci first"
  fi
else
  skip "npm" "npm is not installed"
fi

# --- Chart and secrets ------------------------------------------------------
cd "$root" || exit 1
if have helm; then
  # --set openldap.bindPassword: the chart intentionally fails closed when the
  # naslos-openldap Secret cannot be looked up (helm template/lint have no
  # cluster), so a throwaway value lets the render proceed (AUDIT-H1).
  run "helm lint" helm lint charts/naslos \
    -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml \
    --set openldap.bindPassword=lint-only
  run "authelia ldap password is a Secret file (AUDIT-M3)" check_authelia_ldap_secret
  run "network policy intent (AUDIT-M4)" check_network_policies
else
  skip "helm lint" "helm is not installed"
fi
if have gitleaks; then
  run "gitleaks (tree + history)" gitleaks detect --source=. --no-banner --redact
else
  skip "gitleaks" "not installed (CI uses gitleaks/gitleaks-action)"
fi

printf '\n== summary ==\n'
if [ "$fails" -eq 0 ]; then
  echo "all checks passed"
else
  echo "$fails check(s) failed"
fi
exit "$fails"
