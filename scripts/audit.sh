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
# allow policies, and the agent port must only be open to the API. Inert under
# flannel, but the intent must not silently disappear before a policy CNI lands.
check_network_policies() {
  f=$(mktemp) || return 1
  if ! helm template naslos "$root/charts/naslos" -n naslos \
    -f "$root/charts/naslos/values.yaml" -f "$root/charts/naslos/values-vm.yaml" \
    --set openldap.bindPassword=lint-only >"$f" 2>/dev/null; then
    rm -f "$f"
    return 1
  fi
  ok=1
  grep -q 'name: naslos-default-deny' "$f" || ok=0
  grep -q 'name: naslos-agent-ingress' "$f" || ok=0
  grep -q 'name: naslos-workload-egress' "$f" || ok=0
  # The agent policy exists and selects the API as its only pod peer.
  if ! sed -n '/name: naslos-agent-ingress$/,/^---$/p' "$f" \
    | grep -q "app.kubernetes.io/name: naslos-api"; then
    ok=0
  fi
  rm -f "$f"
  [ "$ok" -eq 1 ]
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
