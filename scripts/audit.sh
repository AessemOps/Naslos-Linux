#!/usr/bin/env bash
# Naslos audit sweep (see docs/AUDIT-2026-09-19-REPORT.md).
#
# Reproducible checks that are fast enough for a pre-PR run. Missing tools are
# reported as "skipped" rather than failing, so it runs anywhere. CI wires this
# into `.github/workflows/ci.yml`; run it by hand before pushing.
#
# Requires bash: the govulncheck advisory-set diff uses process substitution.
#
# Usage:  bash scripts/audit.sh
#         NASLOS_AUDIT_RACE=1 bash scripts/audit.sh   # adds go test -race (CR-06)
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

# AUDIT-M4 regression guard: the LDAP bind password must reach Authelia as an
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

# FR-APP-15 regression guard: the SSO domain list is runtime state. The chart
# must inject it via Authelia's fileContent hooks from a kept seed ConfigMap
# (never static $ssoDomains loops), and the API's live-promotion RBAC must be
# scoped by resourceName so it cannot read authelia-config (which holds the jwt
# secret). Authelia must be a StatefulSet so the API has a deterministic restart
# target.
check_authelia_sso_fragments() {
  f=$(mktemp) || return 1
  if ! helm template naslos "$root/charts/naslos" -n naslos \
    -f "$root/charts/naslos/values.yaml" -f "$root/charts/naslos/values-vm.yaml" \
    --set openldap.bindPassword=lint-only >"$f" 2>/dev/null; then
    rm -f "$f"
    return 1
  fi
  python3 - "$f" <<'PY'
import re, sys, yaml
docs = [d for d in yaml.safe_load_all(open(sys.argv[1])) if d]
def find(kind, name):
    for d in docs:
        if d.get("kind") == kind and d["metadata"]["name"] == name:
            return d
    return None

seed = find("ConfigMap", "naslos-authelia-sso")
assert seed is not None, "missing the naslos-authelia-sso seed ConfigMap"
assert seed["metadata"].get("annotations", {}).get("helm.sh/resource-policy") == "keep", \
    "the SSO seed ConfigMap must be kept across upgrades"
assert "cookies.yml" in seed["data"] and "rules.yml" in seed["data"], \
    "the seed ConfigMap must carry cookies.yml and rules.yml"
# Seed/Fragments parity tripwire: the chart seed is only adopted because the API
# renders the same structure, so any drift must fail here. Values are the VM
# profile's sso.domains (naslos.local).
assert yaml.safe_load(seed["data"]["cookies.yml"]) == [
    {
        "domain": "naslos.local",
        "authelia_url": "https://naslos.local/authelia/",
        "default_redirection_url": "https://naslos.local/",
    },
], f"seed cookies drifted from the API Fragments output: {seed['data']['cookies.yml']!r}"
assert yaml.safe_load(seed["data"]["rules.yml"]) == [
    {"domain": "*.naslos.local", "policy": "one_factor"},
], f"seed rules drifted from the API Fragments output: {seed['data']['rules.yml']!r}"

cfg = find("ConfigMap", "authelia-config")
assert cfg is not None, "missing authelia-config"
configuration = cfg["data"]["configuration.yml"]
assert 'fileContent "/config-sso/cookies.yml"' in configuration, \
    "authelia-config must inject cookies.yml via fileContent"
assert 'fileContent "/config-sso/rules.yml"' in configuration, \
    "authelia-config must inject rules.yml via fileContent"
assert "$ssoDomains" not in configuration, \
    "authelia-config must not render static SSO domain loops"
# With a cookies list Authelia rejects the legacy global default_redirection_url;
# the per-cookie value comes from the fragment (checked above).
body = "\n".join(l for l in configuration.splitlines() if not l.strip().startswith("#"))
assert not re.search(r"^\s*default_redirection_url\s*:", body, re.M), \
    "authelia-config must not set the legacy global default_redirection_url"

sts = find("StatefulSet", "naslos-authelia")
assert sts is not None, "Authelia must be a StatefulSet (a deterministic restart target)"
mounts = [m["mountPath"] for m in sts["spec"]["template"]["spec"]["containers"][0].get("volumeMounts", [])]
assert "/config-sso" in mounts, f"Authelia must mount /config-sso, got {mounts}"

# A promoted domain needs its own portal, served by an API-rendered ExternalName
# route in the apps namespace; Traefik refuses ExternalName services otherwise.
traefik = find("Deployment", "naslos-traefik")
assert traefik is not None, "missing the Traefik Deployment"
args = traefik["spec"]["template"]["spec"]["containers"][0].get("args", [])
assert "--providers.kubernetescrd.allowExternalNameServices=true" in args, \
    "Traefik must allow ExternalName services for the per-domain Authelia portal"

role = find("Role", "naslos-api-authelia-sso")
assert role is not None, "missing the scoped naslos-api-authelia-sso Role"
scoped = {r["resources"][0]: set(r.get("resourceNames", [])) for r in role["rules"]}
assert scoped.get("configmaps") == {"naslos-authelia-sso"}, \
    f"configmaps must be scoped to the fragment ConfigMap: {scoped}"
# The restart is a pod delete, not a workload patch, so the API cannot change
# the Authelia pod spec (which mounts the jwt/LDAP secrets).
assert scoped.get("pods") == {"naslos-authelia-0"}, \
    f"pods must be scoped to the deterministic Authelia pod: {scoped}"
assert not any(r["resources"] == ["statefulsets"] for r in role["rules"]), \
    "the API must not have workload write (a pod-template patch exposes the mounted secrets)"

read = find("Role", "naslos-api-platform-read")
resources = {res for r in read["rules"] for res in r["resources"]}
assert "configmaps" not in resources and "secrets" not in resources, \
    f"platform-read must stay configmap/secret-free: {resources}"
print("authelia SSO fragments + scoped RBAC ok")
PY
  rc=$?
  rm -f "$f"
  [ "$rc" -eq 0 ]
}

# AUDIT-M4 (fresh install) guard: the shipped Talos patch must carry Cilium and
# the flannel/kube-proxy/DNS changes, and match bootstrap/cilium/cilium.yaml.
# A fresh `make bootstrap-vm` renders this file, so a regression here silently
# produces a stock flannel cluster with inert policies.
check_talos_patch() {
  p="$root/bootstrap/vm/naslos-vm.yaml"
  [ -f "$p" ] || return 1
  grep -q 'kind: KubeFlannelCNIConfig' "$p" || return 1
  grep -q '\$patch: delete' "$p" || return 1
  grep -q 'kind: KubeProxyConfig' "$p" || return 1
  grep -q 'forwardKubeDNSToHost: false' "$p" || return 1
  grep -q 'kind: KubeInlineManifestConfig' "$p" || return 1
  grep -q 'name: cilium' "$p" || return 1
  # The spliced block must be exactly current with the checked-in manifest.
  "$root/scripts/render-cilium.sh" --check >/dev/null 2>&1 || return 1
  return 0
}

# PF-M3 guard (also AUDIT-H4): Grafana must stay out of the chart. It shipped a
# committed static admin password; a single `--set grafana.enabled=true` would
# deploy it with `admin` / that password. Removing the dependency, the vendored
# tarball and the values blocks is what makes that impossible.
check_no_grafana() {
  grep -q 'name: grafana' "$root/charts/naslos/Chart.yaml" && return 1
  grep -q 'grafana' "$root/charts/naslos/Chart.lock" && return 1
  if ls "$root"/charts/naslos/charts/grafana-*.tgz >/dev/null 2>&1; then return 1; fi
  grep -q 'grafana' "$root/charts/naslos/values.yaml" && return 1
  grep -q 'grafana' "$root/charts/naslos/values-vm.yaml" && return 1
  return 0
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

# AUDIT-M4 (enforcement): the CiliumNetworkPolicy must exist and allow pods to
# reach the node/API server, or turning enforcement on blocks every pod from
# https://<node>:6443 (verified live). Upstream NetworkPolicy cannot express
# this - Cilium classifies it with the reserved `host` identity.
cnps = {d["metadata"]["name"]: d for d in docs if d.get("kind") == "CiliumNetworkPolicy"}
assert "naslos-allow-host" in cnps, "missing CiliumNetworkPolicy naslos-allow-host"
entities = set()
for r in cnps["naslos-allow-host"]["spec"].get("egress", []):
    entities.update(r.get("toEntities", []))
assert "host" in entities, "allow-host must permit the host entity (API server/kubelet)"
assert "kube-apiserver" in entities, "allow-host must permit the kube-apiserver entity"

# AUDIT-M6: the privileged workloads must be in their own namespace, the API
# must point at the agent in the new namespace, and the API's exec RBAC must
# live where the terminal does. A regression here silently re-merges the split.
# NOTE: the release namespace stays PSA `privileged` on purpose - the API mounts
# hostPath volumes (shares view + buddy receive) and `baseline` forbids them, so
# `naslos` would not schedule its own API pod (verified live).
nss = {d["metadata"]["name"]: d for d in docs if d.get("kind") == "Namespace"}
priv = nss.get("naslos-privileged")
assert priv is not None, "missing naslos-privileged namespace"
assert priv["metadata"]["labels"]["pod-security.kubernetes.io/enforce"] == "privileged", \
    "naslos-privileged must enforce privileged"

def ns_of(d):
    return (d.get("metadata") or {}).get("namespace")

priv_workloads = {
    (d["kind"], d["metadata"]["name"])
    for d in docs
    if d.get("kind") in ("DaemonSet", "Deployment") and ns_of(d) == "naslos-privileged"
}
for kind, name in [
    ("DaemonSet", "naslos-agent"), ("DaemonSet", "naslos-samba"),
    ("DaemonSet", "naslos-nfs"), ("Deployment", "naslos-terminal"),
]:
    assert (kind, name) in priv_workloads, f"{name} is not in naslos-privileged"

# The release namespace must NOT still run OUR hostNetwork/privileged workloads.
# Scoped to this chart's own names (naslos-*): anything else (a user-added
# subchart, say) is not one of the workloads the split moves.
split_names = {"naslos-agent", "naslos-samba", "naslos-nfs", "naslos-terminal"}
for d in docs:
    name = d["metadata"]["name"]
    if (
        d.get("kind") in ("DaemonSet", "Deployment")
        and ns_of(d) == "naslos"
        and name in split_names
    ):
        raise AssertionError(f"{name} must not remain in the release namespace")

# The API must reach the agent in the privileged namespace.
api_env = {}
for d in docs:
    if d.get("kind") == "Deployment" and d["metadata"]["name"] == "naslos-api":
        for e in d["spec"]["template"]["spec"]["containers"][0].get("env", []):
            api_env[e["name"]] = e.get("value")
assert "naslos-privileged.svc" in api_env.get("AGENT_BASE_URL", ""), \
    f"AGENT_BASE_URL does not target the privileged namespace: {api_env.get('AGENT_BASE_URL')!r}"

# PF-H4: the API must receive the LAN CIDR it uses as the default share client
# restriction, so shares are never exported to the world by omission.
assert "NASLOS_LAN_CIDR" in api_env, "the API deployment must set NASLOS_LAN_CIDR (PF-H4)"

# The exec Role/RoleBinding must sit where the terminal pod does, with the API
# (in the release namespace) as the subject.
roles = {(d["kind"], d["metadata"]["name"], ns_of(d)) for d in docs}
assert ("Role", "naslos-api-terminal", "naslos-privileged") in roles, \
    "exec Role must be in naslos-privileged (where the terminal runs)"
assert ("RoleBinding", "naslos-api-terminal", "naslos-privileged") in roles, \
    "exec RoleBinding must be in naslos-privileged"
for d in docs:
    if d.get("kind") == "RoleBinding" and d["metadata"]["name"] == "naslos-api-terminal":
        subjects = d.get("subjects", [])
        assert any(s.get("namespace") == "naslos" for s in subjects), \
            "the RoleBinding subject must be the API in the release namespace"

# Both agent-token Secret copies must exist (secretKeyRef cannot cross namespaces).
secret_ns = {
    ns_of(d) for d in docs
    if d.get("kind") == "Secret" and d["metadata"]["name"] == "naslos-agent"
}
assert secret_ns == {"naslos", "naslos-privileged"}, \
    f"the naslos-agent Secret must exist in both namespaces, got {secret_ns}"

# App-install subsystem: apps get their own namespace (PSA baseline) and the API
# gets a namespaced Role there; the env must be wired for it to work at all.
apps_ns = nss.get("naslos-apps")
assert apps_ns is not None, "missing naslos-apps namespace"
assert apps_ns["metadata"]["labels"]["pod-security.kubernetes.io/enforce"] == "baseline", \
    "naslos-apps must enforce baseline"
assert ("Role", "naslos-api-apps", "naslos-apps") in roles, \
    "the API must have a namespaced Role in naslos-apps"
assert ("RoleBinding", "naslos-api-apps", "naslos-apps") in roles, \
    "the API RoleBinding must be in naslos-apps"
for d in docs:
    if d.get("kind") == "RoleBinding" and d["metadata"]["name"] == "naslos-api-apps":
        assert any(s.get("namespace") == "naslos" for s in d.get("subjects", [])), \
            "the naslos-api-apps binding must target the API in the release namespace"
assert api_env.get("APPS_NAMESPACE") == "naslos-apps", "the API must know the apps namespace"
assert api_env.get("SOURCES_CONFIG"), "the API must get a sources state path"
assert api_env.get("DOMAINS_CONFIG"), "the API must get a domains state path"
# FR-APP-15: live SSO promotion needs the API pointed at the fragment ConfigMap
# and the Authelia workload it restarts.
assert api_env.get("AUTHELIA_SSO_CONFIGMAP") == "naslos-authelia-sso", \
    "the API must target the SSO fragment ConfigMap"
assert api_env.get("AUTHELIA_POD") == "naslos-authelia-0", \
    "the API must target the deterministic Authelia pod to restart"

# Dynamic DNS (FR-DNS): the provider override dir and state path must be wired,
# and the egress rule must stay port-scoped to HTTP/HTTPS (80/443).
assert api_env.get("DDNS_CONFIG"), "the API must get a DDNS state path"
assert api_env.get("DDNS_PROVIDERS_DIR"), "the API must get a DDNS provider override directory"
assert api_env.get("DDNS_IP_SOURCES") and api_env.get("DDNS_IPV6_SOURCES"), \
    "the API must get the public-IP detection source lists"
assert api_env.get("DDNS_UPDATE_COOLDOWN_SECONDS"), "the API must get the DDNS update cooldown"

# Privileged apps namespace: PSA privileged, with its own API Role, for charts
# that need NET_ADMIN (VPN sidecars) and cannot run under baseline.
priv_ns = nss.get("naslos-apps-priv")
assert priv_ns is not None, "missing naslos-apps-priv namespace"
assert priv_ns["metadata"]["labels"]["pod-security.kubernetes.io/enforce"] == "privileged", \
    "naslos-apps-priv must enforce privileged"
assert ("Role", "naslos-api-apps", "naslos-apps-priv") in roles, \
    "the API must have a namespaced Role in naslos-apps-priv"
assert api_env.get("APPS_PRIVILEGED_NAMESPACE") == "naslos-apps-priv", \
    "the API must know the privileged apps namespace"

# Backfill reads Helm labels from workloads rather than listing Secrets; the
# platform read Role must exist and must NOT grant secret access.
assert ("Role", "naslos-api-platform-read", "naslos") in roles, \
    "the API needs read-only workload access in the release namespace for backfill"
for d in docs:
    if d.get("kind") == "Role" and d["metadata"]["name"] == "naslos-api-platform-read":
        resources = [res for rule in d.get("rules", []) for res in rule.get("resources", [])]
        assert "secrets" not in resources, \
            "the platform read Role must not grant secrets (it would expose proxy/LDAP/Authelia secrets)"

# The security-headers middleware must NOT pin HSTS for subdomains: an app
# subdomain can serve a TLS-off route that HSTS would make unreachable.
for d in docs:
    if d.get("kind") == "Middleware" and d["metadata"]["name"] == "security-headers":
        assert "stsIncludeSubdomains" not in d["spec"]["headers"], \
            "security-headers must not set stsIncludeSubdomains"

# FR-DNS-03: the DDNS egress is port-scoped to HTTP/HTTPS and DNS (80/443/53),
# CIDR-scoped, and granted only to the release namespace (the API is the DDNS
# client).
egress_policies = [
    d for d in docs
    if d.get("kind") == "NetworkPolicy" and d["metadata"]["name"] == "naslos-workload-egress"
]
def ddns_rules(policy):
    return [r for r in policy["spec"].get("egress", [])
            if {p.get("port") for p in r.get("ports", [])} == {443, 80, 53}]
ddns_namespaces = [p["metadata"].get("namespace") for p in egress_policies if ddns_rules(p)]
assert ddns_namespaces == ["naslos"], \
    f"the DDNS egress rule must exist only in the release namespace, got {ddns_namespaces}"
for r in ddns_rules(next(p for p in egress_policies if p["metadata"].get("namespace") == "naslos")):
    assert all("ipBlock" in p for p in r.get("to", [])), \
        "the DDNS egress rule must be CIDR-scoped, never a pod selector"
print("network policy intent ok")
PY
  rc=$?
  rm -f "$f"
  [ "$rc" -eq 0 ]
}

# FR-INSTALL: the install pack must build and its metadata.json checksums must
# match every packaged member. Built into a temp dir so the working tree stays
# clean, and the Cilium block must be current with bootstrap/cilium/cilium.yaml.
check_install_pack() {
  scripts/render-installer-template.sh --check >/dev/null 2>&1 || return 1
  d=$(mktemp -d) || return 1
  if ! scripts/build-install-pack.sh "$d" >/dev/null 2>&1; then
    rm -rf "$d"
    return 1
  fi
  python3 - "$d" <<'PY'
import glob, hashlib, json, os, sys, tarfile
d = sys.argv[1]
tars = glob.glob(os.path.join(d, "naslos-install-pack-*.tar.gz"))
assert len(tars) == 1, f"expected exactly one tarball, got {tars}"
with tarfile.open(tars[0]) as tf:
    tf.extractall(os.path.join(d, "x"))
root = glob.glob(os.path.join(d, "x", "naslos-install-pack-*"))[0]
m = json.load(open(os.path.join(root, "metadata.json")))
assert m["talosVersion"] and m["schematicId"], "metadata must pin talosVersion + schematicId"
assert "metal-amd64" in m["isoUrls"], "metadata must carry the metal-amd64 ISO URL"
for rel in ["charts/naslos/values-installer.yaml",
            "machine-config/naslos-installer.yaml.tmpl",
            "manifests/local-path-v0.0.26.yaml",
            "cilium/cilium.yaml",
            "schematic/naslos.yaml"]:
    assert os.path.exists(os.path.join(root, rel)), f"missing pack member {rel}"
for rel, sha in m["checksums"].items():
    p = os.path.join(root, rel)
    assert os.path.exists(p), f"checksum lists a missing member: {rel}"
    assert hashlib.sha256(open(p, "rb").read()).hexdigest() == sha, f"checksum mismatch: {rel}"
print("install pack ok")
PY
  rc=$?
  rm -rf "$d"
  return "$rc"
}

# FR-INSTALL-13 (part 1): the chart's digest mechanism must render. With a
# `sha256:` digest set for every chart image, `helm template` of the installer
# profile must emit `repository@sha256:...` and no longer the tag - this is what
# the digest-pinned pack relies on (VER-2).
check_image_digest_render() {
  hex=$(printf 'a%.0s' $(seq 64))
  f=$(mktemp) || return 1
  if ! helm template naslos "$root/charts/naslos" -n naslos \
    -f "$root/charts/naslos/values.yaml" -f "$root/charts/naslos/values-installer.yaml" \
    --set api.image.digest="sha256:$hex" \
    --set agent.image.digest="sha256:$hex" \
    --set ui.image.digest="sha256:$hex" \
    --set shares.smb.image.digest="sha256:$hex" \
    --set shares.nfs.image.digest="sha256:$hex" \
    --set terminal.image.digest="sha256:$hex" \
    --set openldap.image="ghcr.io/aessemops/naslos-openldap@sha256:$hex" \
    >"$f" 2>/dev/null; then
    rm -f "$f"
    return 1
  fi
  ok=1
  for repo in naslos-api naslos-agent naslos-ui naslos-samba naslos-nfs naslos-terminal naslos-openldap; do
    grep -qF "ghcr.io/aessemops/${repo}@sha256:${hex}" "$f" || ok=0
  done
  # The digest must win over the tag: the installer profile's tag must be gone.
  grep -qF "ghcr.io/aessemops/naslos-api:0.1.0" "$f" && ok=0
  rm -f "$f"
  [ "$ok" -eq 1 ]
}

# FR-INSTALL-13 (part 2): the pack build must pin every chart image by digest
# when a map is supplied, record the same map in metadata.json.images, render,
# and fail closed when a component is missing. This is the test paired with the
# spec MUST.
check_pack_digest_pinning() {
  hex=$(printf 'a%.0s' $(seq 64))
  d=$(mktemp -d) || return 1
  cat >"$d/digests.json" <<EOF
{
  "api": "ghcr.io/aessemops/naslos-api@sha256:${hex}",
  "agent": "ghcr.io/aessemops/naslos-agent@sha256:${hex}",
  "ui": "ghcr.io/aessemops/naslos-ui@sha256:${hex}",
  "samba": "ghcr.io/aessemops/naslos-samba@sha256:${hex}",
  "nfs": "ghcr.io/aessemops/naslos-nfs@sha256:${hex}",
  "terminal": "ghcr.io/aessemops/naslos-terminal@sha256:${hex}",
  "openldap": "ghcr.io/aessemops/naslos-openldap@sha256:${hex}"
}
EOF
  if ! IMAGE_DIGESTS_FILE="$d/digests.json" "$root/scripts/build-install-pack.sh" "$d/ok" >/dev/null 2>&1; then
    rm -rf "$d"
    return 1
  fi
  python3 - "$d/ok" "$hex" <<'PY'
import glob, json, os, sys, tarfile, yaml

d, hex = sys.argv[1], sys.argv[2]
tars = glob.glob(os.path.join(d, "naslos-install-pack-*.tar.gz"))
assert len(tars) == 1, f"expected exactly one tarball, got {tars}"
with tarfile.open(tars[0]) as tf:
    tf.extractall(os.path.join(d, "x"))
root = glob.glob(os.path.join(d, "x", "naslos-install-pack-*"))[0]
m = json.load(open(os.path.join(root, "metadata.json")))
assert m["formatVersion"] == 1, "the images map must be additive (formatVersion stays 1)"
images = m.get("images")
assert isinstance(images, dict) and len(images) == 7, f"metadata.images must pin 7 images: {images!r}"
for comp, ref in images.items():
    assert ref == f"ghcr.io/aessemops/naslos-{comp}@sha256:{hex}", f"bad metadata image {comp}: {ref}"

v = yaml.safe_load(open(os.path.join(root, "charts/naslos/values-installer.yaml")))
for comp, path in [("api", ("api", "image")), ("agent", ("agent", "image")),
                   ("ui", ("ui", "image")), ("samba", ("shares", "smb", "image")),
                   ("nfs", ("shares", "nfs", "image")), ("terminal", ("terminal", "image"))]:
    node = v
    for key in path:
        node = node[key]
    assert node.get("digest") == f"sha256:{hex}", f"{'.'.join(path)}.digest not pinned: {node!r}"
    assert node.get("repository") == f"ghcr.io/aessemops/naslos-{comp}", \
        f"{'.'.join(path)}.repository wrong: {node.get('repository')!r}"
assert v["openldap"]["image"] == f"ghcr.io/aessemops/naslos-openldap@sha256:{hex}", \
    f"openldap.image not pinned: {v['openldap']['image']!r}"
print("pack digest pinning ok")
PY
  rc=$?
  if [ "$rc" -ne 0 ]; then
    rm -rf "$d"
    return "$rc"
  fi
  # A missing component must fail the build (fail closed).
  python3 - "$d/digests.json" "$d/partial.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
d.pop("agent", None)
json.dump(d, open(sys.argv[2], "w"))
PY
  if IMAGE_DIGESTS_FILE="$d/partial.json" "$root/scripts/build-install-pack.sh" "$d/bad" >/dev/null 2>&1; then
    printf 'pack build accepted a partial digest map (must fail closed)\n'
    rm -rf "$d"
    return 1
  fi
  rm -rf "$d"
  return 0
}

# --- Go ---------------------------------------------------------------------
cd "$root/api" || exit 1
run "go vet (api)" go vet ./...
run "go test (api)" go test ./...
# CR-06 fixed: the buddy scheduler's notification race is gone, so the race
# detector runs unconditionally.
run "go test -race (api)" go test -race ./...
if have govulncheck; then
  # govulncheck exits non-zero for ANY findable advisory. The four AUDIT-H3
  # ones are `Fixed in: N/A` and not on an exercised path. The six PF-M2 ones
  # are Go stdlib advisories fixed in 1.26.6, which both go.mod files now
  # require; they are listed so a build under an older toolchain still reports
  # them without failing the gate. A genuinely NEW id fails.
  check_govulncheck() {
    f=$(mktemp) || return 1
    govulncheck ./... >"$f" 2>&1
    found=$(grep -oE '^Vulnerability #[0-9]+: GO-[0-9-]+' "$f" | awk '{print $3}' | sort -u)
    accepted="GO-2026-5064 GO-2026-5338 GO-2026-5622 GO-2026-5932 GO-2026-5026 GO-2026-5972 GO-2026-6089 GO-2026-6090 GO-2026-6091 GO-2026-6218"
    new=$(comm -23 <(printf '%s\n' $found | sort -u) <(printf '%s\n' $accepted | tr ' ' '\n' | sort -u))
    printf 'govulncheck advisories: %s\n' "$(printf '%s' "$found" | tr '\n' ' ')"
    rm -f "$f"
    if [ -n "$new" ]; then
      printf 'NEW advisory (not in the AUDIT-H3 accepted set): %s\n' "$(printf '%s' "$new" | tr '\n' ' ')"
      return 1
    fi
    return 0
  }
  run "govulncheck (api; fails on a NEW advisory)" check_govulncheck
else
  skip "govulncheck (api)" "not installed (go install golang.org/x/vuln/cmd/govulncheck@latest)"
fi
if have gosec; then
  # G101: flags ProxySecretHeader, which is a header NAME, not a credential
  #   (reviewed by hand; annotated #nosec in the source too).
  # G703: path-traversal taint analysis false-positives on buddy/store.go -
  #   the tainted chain names come from os.ReadDir (a directory listing), which
  #   cannot yield '/'-containing or '..' components, and every peer-supplied
  #   name passes validateChainName first (AUDIT-M9). A NEW HIGH rule still
  #   fails this gate.
  run "gosec high severity (api)" gosec -quiet -severity high -exclude G101,G703 ./...
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
  # FR-INSTALL: the installer profile must render (no install-time `fail`) and
  # must not reference the private VM registry.
  run "helm lint (installer profile, FR-INSTALL)" helm lint charts/naslos \
    -f charts/naslos/values.yaml -f charts/naslos/values-installer.yaml
  run "install pack builds + checksums (FR-INSTALL)" check_install_pack
  run "chart images render by digest (FR-INSTALL-13)" check_image_digest_render
  run "install pack pins image digests (FR-INSTALL-13)" check_pack_digest_pinning
  run "authelia ldap password is a Secret file (AUDIT-M3)" check_authelia_ldap_secret
  run "authelia SSO fragments + scoped RBAC (FR-APP-15)" check_authelia_sso_fragments
  run "network policy intent (AUDIT-M4)" check_network_policies
  # AUDIT-M4 (fresh install): bootstrap/vm/naslos-vm.yaml must carry the Cilium
  # inline manifest, flannel deletion, kube-proxy disable and DNS fix, and must
  # be current with bootstrap/cilium/cilium.yaml. Without this a fresh install
  # silently generates stock Talos + flannel (the patch file is what ships).
  run "talos patch ships cilium (AUDIT-M4)" check_talos_patch
  run "grafana stays removed (AUDIT-H4/PF-M3)" check_no_grafana
else
  skip "helm lint" "helm is not installed"
fi
if have gitleaks; then
  # `gitleaks detect` is history-only and was mislabeled "tree + history" here,
  # which let a committed-but-never-committed-later secret slip. Scan both: the
  # working tree (catches new leaks pre-commit) and git history (PF-H1).
  run "gitleaks (working tree)" gitleaks dir . --no-banner --redact
  run "gitleaks (git history)" gitleaks git . --no-banner --redact
else
  skip "gitleaks" "not installed (CI downloads the gitleaks CLI release; or: go install github.com/zricethezav/gitleaks/v8@latest)"
fi

printf '\n== summary ==\n'
if [ "$fails" -eq 0 ]; then
  echo "all checks passed"
else
  echo "$fails check(s) failed"
fi
exit "$fails"
