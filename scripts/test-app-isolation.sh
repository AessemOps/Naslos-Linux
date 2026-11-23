#!/usr/bin/env bash
# Chart assertions for the app-isolation boundary (R-001/R-002/R-003).
#
# Renders the chart with the hardened app-isolation overlay and checks the
# security invariants that a values/template edit could silently undo:
#   - the Helm identity can never write Ingress/NetworkPolicy;
#   - native Kubernetes Ingress is disabled;
#   - the admission policy and its binding exist;
#   - the Authelia session-cookie strip plugin is wired into Traefik.
#
# Usage: bash scripts/test-app-isolation.sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
chart="$root/charts/naslos"

if ! command -v helm >/dev/null 2>&1; then
  echo "helm not found; skipping app-isolation chart assertions" >&2
  exit 0
fi

out=$(mktemp)
trap 'rm -f "$out"' EXIT

helm template naslos "$chart" \
  -f "$chart/values.yaml" \
  -f "$chart/values-vm.yaml" \
  --set openldap.bindPassword=lint-only >"$out"

python3 - "$out" <<'PY'
import sys
import yaml

docs = [d for d in yaml.safe_load_all(open(sys.argv[1])) if d]

def fail(msg):
    print(f"FAIL: {msg}")
    raise SystemExit(1)

def by_kind(kind):
    return [d for d in docs if d.get("kind") == kind]

# R-001: the reduced Helm identity has no networking writes.
for kind in ("Role", "ClusterRole"):
    for role in by_kind(kind):
        if role.get("metadata", {}).get("name", "") not in (
            "naslos-helm-apps",
            "naslos-helm-workloads",
        ):
            continue
        for rule in role.get("rules", []):
            groups = rule.get("apiGroups", [])
            resources = rule.get("resources", [])
            if "networking.k8s.io" in groups and (
                "ingresses" in resources or "networkpolicies" in resources
            ):
                fail(f"{kind}/{role['metadata']['name']} still grants {resources}")

# R-001: the admission policy denies Helm Ingress/NetworkPolicy and namespace
# spoofing, and is enforced (Deny), not audit-only.
policies = {d["metadata"]["name"]: d for d in by_kind("ValidatingAdmissionPolicy")}
binding = {
    d["metadata"]["name"]: d
    for d in by_kind("ValidatingAdmissionPolicyBinding")
}
pol = policies.get("naslos-helm-app-isolation")
if not pol:
    fail("ValidatingAdmissionPolicy/naslos-helm-app-isolation is missing")
ex = [v.get("expression", "") for v in pol.get("spec", {}).get("validations", [])]
if not any("networking.k8s.io" in e and "ingresses" in e for e in ex):
    fail("admission policy does not deny Helm Ingress/NetworkPolicy writes")
if not any("request.namespace" in e for e in ex):
    fail("admission policy does not reject cross-namespace metadata")
b = binding.get("naslos-helm-app-isolation")
if not b:
    fail("ValidatingAdmissionPolicyBinding/naslos-helm-app-isolation is missing")
if "Deny" not in b.get("spec", {}).get("validationActions", []):
    fail("admission policy binding is not Deny-enforced")

# R-001: native Ingress provider/class is off; the CRD provider still serves
# the API's IngressRoutes and allows cross-namespace Service targets.
args = []
for dep in by_kind("Deployment"):
    spec = dep.get("spec", {}).get("template", {}).get("spec", {})
    for c in spec.get("containers", []):
        if "traefik" in c.get("image", ""):
            args.extend(c.get("args", []))
if not args:
    fail("traefik deployment not found")
if any(a.startswith("--providers.kubernetesingress") for a in args):
    fail("traefik still enables the native Ingress provider")
if "--providers.kubernetescrd.allowCrossNamespace=true" not in args:
    fail("traefik CRD provider does not allow cross-namespace Services")
if not any(a.startswith("--providers.kubernetescrd") for a in args):
    fail("traefik CRD provider is not enabled")
if by_kind("IngressClass"):
    fail("an IngressClass is still created")

# R-003: the local plugin that strips the Authelia session cookie is declared
# and its source is shipped by the chart for Traefik to load.
if not any(
    a.startswith("--experimental.localPlugins.naslos-cookie-strip.moduleName=")
    and a.endswith("naslos.local/cookiestrip")
    for a in args
):
    fail("traefik does not load the naslos-cookie-strip local plugin")
plugin_cms = [
    c
    for c in by_kind("ConfigMap")
    if "local-plugin-naslos-cookie-strip" in c.get("metadata", {}).get("name", "")
]
if not plugin_cms:
    fail("the local plugin ConfigMap is missing")
if ".traefik.yml" not in plugin_cms[0].get("data", {}):
    fail("the local plugin manifest (.traefik.yml) is missing")
if "plugin.go" not in plugin_cms[0].get("data", {}):
    fail("the local plugin source (plugin.go) is missing")

print("app-isolation chart assertions: ok")
PY

# R-003: execute the shipped plugin source when a Go toolchain is available.
# The plugin is interpreted by Traefik at runtime, so a compile-only check is
# not enough: drive it with a request and assert the cookie handling.
plugin_dir=$(mktemp -d)
trap 'rm -rf "$plugin_dir"; rm -f "$out"' EXIT
python3 - "$out" "$plugin_dir" <<'PY'
import sys
import yaml

docs = [d for d in yaml.safe_load_all(open(sys.argv[1])) if d]
for d in docs:
    name = d.get("metadata", {}).get("name", "")
    if d.get("kind") == "ConfigMap" and "local-plugin-naslos-cookie-strip" in name:
        for key, value in d.get("data", {}).items():
            with open(f"{sys.argv[2]}/{key}", "w") as fh:
                fh.write(value)
        break
PY
cat >"$plugin_dir/plugin_test.go" <<'GO'
package cookiestrip

import (
	"context"
	"net/http"
	"net/http/httptest"
	"testing"
)

func run(t *testing.T, cookie string) string {
	t.Helper()
	var got string
	next := http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		got = r.Header.Get("Cookie")
	})
	h, err := New(context.Background(), next, CreateConfig(), "test")
	if err != nil {
		t.Fatal(err)
	}
	req := httptest.NewRequest(http.MethodGet, "http://app.example/", nil)
	if cookie != "" {
		req.Header.Set("Cookie", cookie)
	}
	h.ServeHTTP(httptest.NewRecorder(), req)
	return got
}

func TestStripsOnlyTheAutheliaSessionCookie(t *testing.T) {
	if got := run(t, "authelia_session=abc; theme=dark"); got != "theme=dark" {
		t.Fatalf("cookie = %q, want %q", got, "theme=dark")
	}
}

func TestRemovesTheHeaderWhenOnlyTheSessionCookieIsPresent(t *testing.T) {
	if got := run(t, "authelia_session=abc"); got != "" {
		t.Fatalf("cookie = %q, want empty", got)
	}
}

func TestLeavesOtherCookiesAlone(t *testing.T) {
	if got := run(t, "theme=dark"); got != "theme=dark" {
		t.Fatalf("cookie = %q, want %q", got, "theme=dark")
	}
}
GO
if command -v go >/dev/null 2>&1; then
  (cd "$plugin_dir" && GO111MODULE=on GOFLAGS=-mod=mod go test ./... >/dev/null)
  echo "cookie-strip plugin tests: ok"
else
  echo "go not found; skipped cookie-strip plugin execution test" >&2
fi
