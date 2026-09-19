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

# --- Go ---------------------------------------------------------------------
cd "$root/api" || exit 1
run "go vet (api)" go vet ./...
run "go test (api)" go test ./...
if [ "${NASLOS_AUDIT_RACE:-0}" = "1" ]; then
  run "go test -race (api)" go test -race ./...
else
  skip "go test -race (api)" "set NASLOS_AUDIT_RACE=1 (CR-06 still open)"
fi
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
run "go test (agent)" go test ./...

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
