#!/usr/bin/env bash
#
# deploy-vm.sh — deploy Naslos to the single-node Talos VM at 192.168.1.96.
#
# This script is meant to be run from the repo root on a machine that has
# talosctl, kubectl, helm and (optionally) docker installed.
#
# Environment variables:
#   VM_IP          Target Talos node IP (default: 192.168.1.96)
#   REGISTRY       Container registry for NasOS images (default: 192.168.1.2:30095)
#   REGISTRY_HTTP_SECRET  HTTP basic-auth password for the registry (default: secret)
#   IMAGE_TAG      Image tag (default: 0.1.0)
#   SKIP_BOOTSTRAP Set to "1" to skip talosctl bootstrap (default: 0)
#   SKIP_IMAGES    Set to "1" to skip docker build/push (images already in registry)
#   WIPE_STATE     Set to "1" to delete generated Talos PKI/config for a clean
#                  reinstall (only with a freshly wiped VM; otherwise orphans node)
#   HELM_FLAGS     Extra helm flags.

set -euo pipefail

VM_IP="${VM_IP:-192.168.1.96}"
REGISTRY="${REGISTRY:-192.168.1.2:30095}"
REGISTRY_HTTP_SECRET="${REGISTRY_HTTP_SECRET:-secret}"
IMAGE_TAG="${IMAGE_TAG:-0.1.0}"
SKIP_BOOTSTRAP="${SKIP_BOOTSTRAP:-0}"
SKIP_IMAGES="${SKIP_IMAGES:-0}"
HELM_FLAGS="${HELM_FLAGS:-}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

TALOSCONFIG="$REPO_ROOT/bootstrap/vm/talosconfig"
CONTROLPLANE="$REPO_ROOT/bootstrap/vm/controlplane.yaml"

echo "=== NasOS VM deployment ==="
echo "VM IP:        $VM_IP"
echo "Registry:     $REGISTRY"
echo "Image tag:    $IMAGE_TAG"
echo "Repo root:    $REPO_ROOT"

# --- sanity checks ---
for tool in talosctl kubectl helm; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "ERROR: required tool '$tool' is not installed" >&2
        exit 1
    fi
done

# --- configure docker for the private HTTP registry (best-effort, never fatal) ---
# Tracks whether we modified the daemon config so we can restart the daemon.
DOCKER_DAEMON_CHANGED=0
if command -v docker >/dev/null 2>&1; then
    if [[ "$REGISTRY" == *":"* && ! "$REGISTRY" =~ ^.*\.docker\.io.* ]]; then
        echo "=== Ensuring Docker trusts insecure registry $REGISTRY ==="
        DOCKER_DAEMON_FILE="/etc/docker/daemon.json"
        # Already configured? Then nothing to do.
        if [ -f "$DOCKER_DAEMON_FILE" ] && grep -q "$REGISTRY" "$DOCKER_DAEMON_FILE" 2>/dev/null; then
            echo "Docker already lists $REGISTRY as insecure-registry."
        elif ! command -v jq >/dev/null 2>&1; then
            echo "WARNING: jq not found; cannot merge $DOCKER_DAEMON_FILE automatically." >&2
            echo "         Add manually: {\"insecure-registries\": [\"$REGISTRY\"]} then restart docker." >&2
        else
            # Need root to write /etc/docker/daemon.json — use sudo when available,
            # otherwise just print instructions and continue (do NOT abort the deploy).
            SUDO=""
            if [ "$(id -u)" -ne 0 ]; then
                if command -v sudo >/dev/null 2>&1; then
                    SUDO="sudo"
                else
                    echo "WARNING: cannot write $DOCKER_DAEMON_FILE (not root, no sudo)." >&2
                    echo "         Ask an admin to add: {\"insecure-registries\": [\"$REGISTRY\"]}" >&2
                    echo "         then 'systemctl restart docker'. Continuing anyway..." >&2
                fi
            fi
            if [ -n "$SUDO" ] || [ "$(id -u)" -eq 0 ]; then
                $SUDO mkdir -p "$(dirname "$DOCKER_DAEMON_FILE")" || echo "WARNING: mkdir $(dirname "$DOCKER_DAEMON_FILE") failed; continuing." >&2
                if [ -f "$DOCKER_DAEMON_FILE" ]; then
                    if jq --arg reg "$REGISTRY" '. + {"insecure-registries": ((.["insecure-registries"] // []) + [$reg] | unique)}' \
                            "$DOCKER_DAEMON_FILE" 2>/dev/null | $SUDO tee "$DOCKER_DAEMON_FILE.tmp" >/dev/null; then
                        $SUDO mv "$DOCKER_DAEMON_FILE.tmp" "$DOCKER_DAEMON_FILE" \
                            || echo "WARNING: could not update $DOCKER_DAEMON_FILE; continuing." >&2
                        echo "Docker daemon.json updated. Restarting docker daemon to apply..."
                        DOCKER_DAEMON_CHANGED=1
                    else
                        echo "WARNING: $DOCKER_DAEMON_FILE is not valid JSON; left untouched." >&2
                        echo "         Add \"$REGISTRY\" to insecure-registries manually, then restart docker." >&2
                    fi
                else
                    if jq -n --arg reg "$REGISTRY" '{"insecure-registries": [$reg]}' 2>/dev/null | $SUDO tee "$DOCKER_DAEMON_FILE" >/dev/null; then
                        echo "Docker daemon.json created. Restarting docker daemon to apply..."
                        DOCKER_DAEMON_CHANGED=1
                    else
                        echo "WARNING: could not create $DOCKER_DAEMON_FILE; continuing." >&2
                    fi
                fi
            fi
        fi
        # A daemon.json change only takes effect after the daemon restarts.
        # Without the restart, `docker push` fails with:
        #   "http: server gave HTTP response to HTTPS client".
        if [ "$DOCKER_DAEMON_CHANGED" = "1" ]; then
            if [ -n "${SUDO:-}" ] || [ "$(id -u)" -eq 0 ]; then
                ${SUDO:-} systemctl restart docker 2>/dev/null \
                    || ${SUDO:-} service docker restart 2>/dev/null \
                    || echo "WARNING: could not restart docker automatically; run 'sudo systemctl restart docker' manually, then re-run this script." >&2
                # Give the daemon a moment to come back before pushing.
                for _ in $(seq 1 15); do
                    docker info >/dev/null 2>&1 && break
                    sleep 2
                done
            else
                echo "WARNING: docker config changed but daemon not restarted (no sudo)." >&2
                echo "         Run 'sudo systemctl restart docker' manually, then re-run this script." >&2
            fi
        fi
        # Final sanity check: the running daemon must report the registry as insecure.
        if docker info >/dev/null 2>&1; then
            if ! docker info 2>/dev/null | grep -q "$REGISTRY"; then
                echo "WARNING: running docker daemon does not list $REGISTRY as insecure." >&2
                echo "         Push will fail with 'server gave HTTP response to HTTPS client'." >&2
                echo "         Fix: ensure $DOCKER_DAEMON_FILE contains {\"insecure-registries\": [\"$REGISTRY\"]}," >&2
                echo "         then 'sudo systemctl restart docker' and re-run this script." >&2
            fi
        fi
    fi
fi

# --- build & push images (optional) ---
if [[ "$SKIP_IMAGES" == "1" ]]; then
    echo "=== Skipping image build/push (SKIP_IMAGES=1; assuming $REGISTRY already has :$IMAGE_TAG) ==="
elif command -v docker >/dev/null 2>&1; then
    echo "=== Building NasOS container images ==="
    make images REGISTRY="$REGISTRY" IMAGE_TAG="$IMAGE_TAG"

    echo "=== Pushing images to $REGISTRY ==="
    echo "(If this fails, ensure docker is configured for the registry and you are logged in.)"
    # Login to the HTTP registry (empty username, password from REGISTRY_HTTP_SECRET)
    if [ -n "$REGISTRY_HTTP_SECRET" ]; then
        echo "$REGISTRY_HTTP_SECRET" | docker login "$REGISTRY" --username "" --password-stdin 2>/dev/null || {
            echo "Retrying docker login with placeholder username '_' ..."
            echo "$REGISTRY_HTTP_SECRET" | docker login "$REGISTRY" --username "_" --password-stdin || true
        }
    fi
    make push-images REGISTRY="$REGISTRY" IMAGE_TAG="$IMAGE_TAG" || {
        echo "WARNING: image push failed; the VM may not be able to pull images." >&2
        echo "         If the images are already present in the registry, you can ignore this." >&2
    }
else
    echo "WARNING: docker not found; skipping image build/push." >&2
fi

# --- wipe local state for a clean reinstall (opt-in) ---
# WIPE_STATE=1 deletes the generated Talos PKI/machine config so the next
# section generates ONE fresh set. Use this only when you are about to wipe
# and reinstall the VM itself (fresh /dev/vda from ISO); otherwise the new
# PKI will orphan the already-installed node ("unknown authority" at
# bootstrap). Safe to combine with SKIP_* flags below.
if [[ "${WIPE_STATE:-0}" == "1" ]]; then
    echo "=== WIPE_STATE=1: removing generated Talos state ==="
    rm -f "$REPO_ROOT/bootstrap/vm/talosconfig" "$REPO_ROOT/bootstrap/vm/controlplane.yaml"
fi

# --- generate VM bootstrap config (only on fresh nodes) ---
# `make bootstrap-vm` regenerates all PKI; re-running it against an installed
# node orphans talosconfig (bootstrap then fails with "unknown authority").
# The deploy script therefore skips regeneration when talosconfig already
# exists — delete bootstrap/vm/talosconfig + controlplane.yaml explicitly if
# you really want to start over on a FRESH (wiped) node.
echo "=== Talos bootstrap config ==="
if [ -f "$REPO_ROOT/bootstrap/vm/talosconfig" ]; then
    echo "Reusing existing bootstrap/vm/talosconfig (node already installed)."
    echo "To regenerate PKI for a FRESH node, delete bootstrap/vm/talosconfig and re-run."
else
    echo "=== Generating Talos bootstrap config ==="
    make bootstrap-vm VM_IP="$VM_IP"
fi

# --- apply machine config to the running Talos node ---
echo "=== Applying machine configuration to $VM_IP ==="
export TALOSCONFIG="$TALOSCONFIG"
# Freshly-booted installer images accept --insecure; once installed, Talos
# requires authenticated apply-config. Try insecure first (installer mode),
# fall back to authenticated (installed mode). Either way, capture output so
# we can decide whether a reboot/install was triggered.
APPLY_OUT=""
if APPLY_OUT="$(talosctl apply-config --insecure --nodes "$VM_IP" --file "$CONTROLPLANE" 2>&1)"; then
    echo "$APPLY_OUT"
else
    echo "$APPLY_OUT" >&2
    if echo "$APPLY_OUT" | grep -qiE "connection refused|Unavailable|transport"; then
        echo "Insecure apply failed (node likely already installed); retrying with authenticated apply-config..."
        if ! APPLY_OUT="$(talosctl apply-config --nodes "$VM_IP" --endpoints "$VM_IP" --file "$CONTROLPLANE" 2>&1)"; then
            echo "$APPLY_OUT" >&2
            echo "ERROR: talosctl apply-config failed; not attempting bootstrap." >&2
            exit 1
        fi
        echo "$APPLY_OUT"
    else
        echo "ERROR: talosctl apply-config failed; not attempting bootstrap." >&2
        exit 1
    fi
fi

# If the node did NOT reboot, it only reloaded config (already installed) and
# is ready for bootstrap right away. If it is (re)installing, apid goes away
# during install + reboot — wait for it to come back before bootstrapping.
if echo "$APPLY_OUT" | grep -qi "without a reboot"; then
    echo "Config applied without reboot; proceeding to bootstrap."
else
    echo "Install/reboot triggered; waiting for Talos API ($VM_IP:50000) to return..."
    echo "(A fresh install can take 10-20 min: installer image pull + install + reboot.)"
    API_BACK=0
    for i in $(seq 1 240); do
        if (echo > "/dev/tcp/$VM_IP/50000") >/dev/null 2>&1; then
            echo "Talos API is back (after ~$((i * 5))s)."
            API_BACK=1
            break
        fi
        if (( i % 12 == 0 )); then
            echo "  still waiting... ~$((i / 12)) min elapsed"
        fi
        sleep 5
    done
    if [ "$API_BACK" -ne 1 ]; then
        echo "WARNING: Talos API did not come back within 20 minutes." >&2
        echo "       The node is probably still installing — check the VM console." >&2
        echo "       Once the console shows the INSTALLED system (a talos-xxxx hostname and" >&2
        echo "       'etcd is waiting to join the cluster'), re-run this script WITHOUT" >&2
        echo "       WIPE_STATE (and with SKIP_IMAGES=1 to skip the rebuild):" >&2
        echo "         SKIP_IMAGES=1 ./scripts/deploy-vm.sh" >&2
        exit 1
    fi
    # Extra grace period: apid can accept TCP before it serves bootstrap RPCs.
    sleep 15
fi

# --- bootstrap etcd / Kubernetes (idempotent) ---
# bootstrap can only succeed once per cluster; if a previous run already
# bootstrapped etcd, the server returns AlreadyExists/already bootstrapped —
# treat that as success and continue to health/kubeconfig.
if [[ "$SKIP_BOOTSTRAP" == "1" ]]; then
    echo "=== Skipping bootstrap (SKIP_BOOTSTRAP=1) ==="
else
echo "=== Bootstrapping Kubernetes control plane ==="
BOOTSTRAP_OUT="$(talosctl bootstrap --nodes "$VM_IP" --endpoints "$VM_IP" 2>&1)" && BOOTSTRAP_RC=0 || BOOTSTRAP_RC=$?
echo "$BOOTSTRAP_OUT"
if [ "$BOOTSTRAP_RC" -ne 0 ]; then
    if echo "$BOOTSTRAP_OUT" | grep -qiE "already (exists|bootstrapped)|AlreadyExists"; then
        echo "Cluster already bootstrapped; continuing."
    elif echo "$BOOTSTRAP_OUT" | grep -qiE "connection refused|Unavailable|transport"; then
        echo "ERROR: Talos API not reachable for bootstrap." >&2
        echo "       The node is likely still installing/rebooting, or the install failed." >&2
        echo "       Check the VM console. Once it shows the installed system" >&2
        echo "       (talos-xxxx hostname + 'etcd is waiting to join the cluster'), re-run:" >&2
        echo "         SKIP_IMAGES=1 ./scripts/deploy-vm.sh" >&2
        echo "       (Do NOT use SKIP_BOOTSTRAP=1 here — bootstrap has not happened yet.)" >&2
        exit 1
    else
        echo "$BOOTSTRAP_OUT" >&2
        exit 1
    fi
fi
fi

# --- wait for Talos to become healthy ---
# NOTE: always pass --nodes/--endpoints explicitly. The generated talosconfig
# has no global endpoint set, so bare `talosctl health` fails with
# "failed to determine endpoints" — that error is about CLI flags, not the VM.
echo "=== Waiting for Talos node health ==="
talosctl --nodes "$VM_IP" --endpoints "$VM_IP" health || true

# --- retrieve Kubernetes credentials ---
echo "=== Fetching Kubernetes kubeconfig ==="
talosctl kubeconfig --nodes "$VM_IP" --endpoints "$VM_IP" -f "$HOME/.kube/config" || true

# --- deploy local-path-provisioner (works on single-node VM without a ZFS pool) ---
echo "=== Deploying local-path-provisioner ==="
kubectl apply -f https://raw.githubusercontent.com/rancher/local-path-provisioner/v0.0.26/deploy/local-path-storage.yaml

# Wait for the provisioner to become ready
kubectl wait --for=condition=ready pod -l app=local-path-provisioner -n local-path-storage --timeout=120s || true

# PodSecurity: the local-path helper pod uses hostPath volumes, which the
# default "baseline" PodSecurity admission forbids — PVC provisioning fails
# with `violates PodSecurity "baseline:latest": hostPath volumes` unless the
# provisioner's namespace is privileged.
kubectl label ns local-path-storage \
    pod-security.kubernetes.io/enforce=privileged \
    pod-security.kubernetes.io/enforce-version=latest --overwrite || true

# Make local-path the cluster default StorageClass so chart PVCs
# (prometheus, alertmanager, ...) bind without an explicit storageClassName.
kubectl patch storageclass local-path \
    -p '{"metadata":{"annotations":{"storageclass.kubernetes.io/is-default-class":"true"}}}' \
    || echo "WARN: could not patch local-path as default StorageClass" >&2

# --- create OpenLDAP secrets and deploy OpenLDAP ---
echo "=== Creating OpenLDAP secrets ==="
kubectl create namespace naslos --dry-run=client -o yaml | kubectl apply -f -
# PodSecurity: this namespace runs privileged workloads (naslos-agent mounts
# host devices) — relax admission from the default baseline/restricted.
kubectl label namespace naslos \
    pod-security.kubernetes.io/enforce=privileged \
    pod-security.kubernetes.io/enforce-version=latest --overwrite || true
# Adopt the namespace for Helm so `helm upgrade --install ... --create-namespace`
# doesn't fail with "invalid ownership metadata" on the already-existing ns.
for label in "app.kubernetes.io/managed-by=Helm"; do
    kubectl label namespace naslos "$label" --overwrite 2>/dev/null \
        || kubectl label --overwrite namespace naslos "$label"
done
kubectl annotate namespace naslos "meta.helm.sh/release-name=naslos" --overwrite 2>/dev/null || true
kubectl annotate namespace naslos "meta.helm.sh/release-namespace=naslos" --overwrite 2>/dev/null || true
LDAP_IMAGE="$REGISTRY/naslos-openldap:$IMAGE_TAG" ./openldap/generate-secrets.sh

echo "=== Deploying OpenLDAP ==="
# Standalone manifests use a fixed image; substitute the target registry/tag
# (they live outside the Helm chart, so --set openldap.image would not apply).
LDAP_MANIFEST_DIR="$(mktemp -d)"
trap 'rm -rf "$LDAP_MANIFEST_DIR"' EXIT
for f in openldap/manifests/*.yaml; do
    case "$(basename "$f")" in
        secrets.yaml) continue ;;
    esac
    sed -e "s|image: .*naslos-openldap:[^[:space:]]*|image: $REGISTRY/naslos-openldap:$IMAGE_TAG|" \
        "$f" > "$LDAP_MANIFEST_DIR/$(basename "$f")"
done
kubectl apply -f "$LDAP_MANIFEST_DIR"

# --- install NasOS Helm chart ---
echo "=== Installing NasOS on the VM ==="
make install-vm VM_IP="$VM_IP" HELM_FLAGS="$HELM_FLAGS \
  --set api.image.repository=$REGISTRY/naslos-api \
  --set api.image.tag=$IMAGE_TAG \
  --set agent.image.repository=$REGISTRY/naslos-agent \
  --set agent.image.tag=$IMAGE_TAG \
  --set ui.image.repository=$REGISTRY/naslos-ui \
  --set ui.image.tag=$IMAGE_TAG"

echo ""
echo "=== Deployment complete ==="
echo "UI should become available at http://$VM_IP:30080"
echo "Watch progress with: kubectl get pods -n naslos -w"