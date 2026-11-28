#!/usr/bin/env bash
# Cloud backup drill: send -> interrupt -> resume -> restore -> verify.
#
# THIS SCRIPT IS NOT RUN BY CI AND MUST NOT TOUCH A REAL BUCKET. It temporarily
# relaxes the API's cloud address policy and adds a drill-only egress rule so a
# throwaway MinIO inside the cluster can act as the S3 endpoint; both changes are
# reverted in the EXIT trap. Read it before running it on any node.
#
#   DRILL_CONFIRM=yes KUBECONFIG=... ./scripts/drill-cloud-backup.sh
#
# Optional:
#   MINIO_ENDPOINT=http://192.168.1.50:9000  use an existing LAN MinIO instead
#                                            (then set cloudPrivateEndpoints in
#                                            the chart yourself; the script
#                                            skips the in-cluster deploy)
#   DRILL_POOL=tank                          pool for the throwaway datasets
#   DRILL_SIZE_MB=256                        payload size (bigger = easier to
#                                            interrupt mid-stream)
set -euo pipefail

NAMESPACE="${NAMESPACE:-naslos}"
PRIV_NAMESPACE="${PRIV_NAMESPACE:-naslos-privileged}"
APPS_NAMESPACE="${APPS_NAMESPACE:-naslos-apps}"
TERMINAL_DEPLOY="${TERMINAL_DEPLOY:-naslos-terminal}"
API_SERVICE="${API_SERVICE:-http://naslos-api.${NAMESPACE}.svc.cluster.local:8080}"
MINIO_NAME="${MINIO_NAME:-naslos-cloud-drill-minio}"
MINIO_ACCESS="${MINIO_ACCESS:-}"
MINIO_SECRET="${MINIO_SECRET:-}"
MINIO_BUCKET="${MINIO_BUCKET:-naslos-cloud-drill}"
DRILL_PREFIX="${DRILL_PREFIX:-drill}"
DRILL_SOURCE_NAME="${DRILL_SOURCE_NAME:-naslos-cloud-drill}"
DRILL_RESTORE_NAME="${DRILL_RESTORE_NAME:-naslos-cloud-drill-restore}"
DRILL_SIZE_MB="${DRILL_SIZE_MB:-256}"
KEEP="${KEEP:-0}"

say() { printf '\n== %s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

[[ "${DRILL_CONFIRM:-}" == "yes" ]] || die "refusing to run: set DRILL_CONFIRM=yes (read the header first)"
command -v kubectl >/dev/null || die "kubectl is required"
command -v jq >/dev/null || die "jq is required"
command -v curl >/dev/null || die "curl is required (used from the terminal pod)"
kubectl -n "$NAMESPACE" get deploy/naslos-api >/dev/null || die "cannot reach the NASLOS API deployment"

TMP="$(mktemp -d)"
trap 'cleanup' EXIT

ORIG_PRIVATE_CIDRS=""
ORIG_BLOCKED_CIDRS=""
API_PATCHED=0
MINIO_CREATED=0
DEST_ID=""
SOURCE_DATASET=""
RESTORE_DATASET=""

cleanup() {
  set +e
  say "cleanup"
  if [[ "${KEEP:-0}" != "1" ]]; then
    if [[ -n "$DEST_ID" ]]; then
      api DELETE "/api/cloud/destinations/${DEST_ID}" >/dev/null 2>&1
    fi
    if [[ -n "$RESTORE_DATASET" ]]; then
      api DELETE "/api/datasets?name=${RESTORE_DATASET//\//%2F}&recursive=true&confirm=${RESTORE_DATASET//\//%2F}" >/dev/null 2>&1
    fi
    if [[ -n "$SOURCE_DATASET" ]]; then
      api DELETE "/api/datasets?name=${SOURCE_DATASET//\//%2F}&recursive=true&confirm=${SOURCE_DATASET//\//%2F}" >/dev/null 2>&1
    fi
  else
    say "KEEP=1: leaving the drill destination, datasets and MinIO in place"
  fi
  # The address-policy and drill-egress changes are never kept: a widened
  # allowlist must not outlive the drill.
  if [[ "$API_PATCHED" == "1" ]]; then
    say "restoring the API cloud address policy"
    kubectl -n "$NAMESPACE" set env deploy/naslos-api \
      CLOUD_PRIVATE_CIDRS="$ORIG_PRIVATE_CIDRS" \
      CLOUD_BLOCKED_CIDRS="$ORIG_BLOCKED_CIDRS" >/dev/null 2>&1
    kubectl -n "$NAMESPACE" rollout status deploy/naslos-api --timeout=300s >/dev/null 2>&1
  fi
  kubectl -n "$NAMESPACE" delete networkpolicy naslos-api-cloud-egress-drill --ignore-not-found >/dev/null 2>&1
  if [[ "${KEEP:-0}" != "1" && "$MINIO_CREATED" == "1" ]]; then
    kubectl -n "$APPS_NAMESPACE" delete deploy "$MINIO_NAME" --ignore-not-found >/dev/null 2>&1
    kubectl -n "$APPS_NAMESPACE" delete svc "$MINIO_NAME" --ignore-not-found >/dev/null 2>&1
    kubectl -n "$APPS_NAMESPACE" delete secret "$MINIO_NAME" --ignore-not-found >/dev/null 2>&1
    kubectl -n "$APPS_NAMESPACE" delete pvc "$MINIO_NAME" --ignore-not-found >/dev/null 2>&1
  fi
  rm -rf "$TMP"
}

# The API is owner-gated and restricted to the proxy/pod CIDR: call it from the
# trusted terminal pod, exactly as the documentation says.
PROXY_SECRET="$(kubectl -n "$NAMESPACE" get secret naslos-proxy -o jsonpath='{.data.secret}' | base64 -d)"
api() {
  local method="$1" path="$2" body="${3:-}"
  local args=(-sS -X "$method" "${API_SERVICE}${path}"
    -H "Remote-User: cloud-drill"
    -H "Remote-Groups: naslos_admins"
    -H "X-Naslos-Proxy-Secret: ${PROXY_SECRET}"
    -H "Content-Type: application/json")
  if [[ -n "$body" ]]; then
    args+=(-d "$body")
  fi
  kubectl -n "$PRIV_NAMESPACE" exec "deploy/${TERMINAL_DEPLOY}" -- curl "${args[@]}"
}

api_ok() {
  local method="$1" path="$2" body="${3:-}"
  local out
  out="$(api "$method" "$path" "$body")"
  jq -e 'if type == "object" and has("error") then false else true end' >/dev/null <<<"$out" \
    || die "$method $path failed: $out"
  printf '%s' "$out"
}

wait_job() {
  local id="$1" timeout="${2:-3600}" started=$SECONDS state
  while (( SECONDS - started < timeout )); do
    state="$(api_ok GET "/api/cloud/jobs/${id}" | jq -r '.state')"
    case "$state" in
      succeeded) api_ok GET "/api/cloud/jobs/${id}"; return 0 ;;
      failed|cancelled) die "job ${id} ended ${state}: $(api_ok GET "/api/cloud/jobs/${id}" | jq -c '{error,result}')" ;;
      running) sleep 2 ;;
      *) die "unexpected job state ${state}" ;;
    esac
  done
  die "job ${id} did not finish within ${timeout}s"
}

say "preconditions"
POOL="${DRILL_POOL:-$(api_ok GET /api/volumes/zfs | jq -r '.[0].name // .pools[0].name // empty')}"
[[ -n "$POOL" ]] || die "no pool found; set DRILL_POOL"
SOURCE_DATASET="${POOL}/${DRILL_SOURCE_NAME}"
RESTORE_DATASET="${POOL}/${DRILL_RESTORE_NAME}"
api_ok GET /api/cloud/destinations >/dev/null || die "cloud routes are not available"

if [[ -n "${MINIO_ENDPOINT:-}" ]]; then
  ENDPOINT="$MINIO_ENDPOINT"
  [[ -n "$MINIO_ACCESS" && -n "$MINIO_SECRET" ]] || die "MINIO_ENDPOINT needs MINIO_ACCESS and MINIO_SECRET"
  say "using the operator-provided MinIO at ${ENDPOINT} (ensure cloudPrivateEndpoints allows it)"
else
  say "deploying throwaway MinIO in ${APPS_NAMESPACE}"
  MINIO_USER="drill$(head -c 8 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  MINIO_PASS="$(head -c 24 /dev/urandom | base64 | tr -d '=+/')"
  MINIO_ACCESS="$MINIO_USER"
  MINIO_SECRET="$MINIO_PASS"
  kubectl -n "$APPS_NAMESPACE" create secret generic "$MINIO_NAME" \
    --from-literal=MINIO_ROOT_USER="$MINIO_USER" \
    --from-literal=MINIO_ROOT_PASSWORD="$MINIO_PASS" >/dev/null
  cat <<YAML | kubectl apply -f - >/dev/null
apiVersion: v1
kind: PersistentVolumeClaim
metadata: {name: ${MINIO_NAME}, namespace: ${APPS_NAMESPACE}}
spec:
  accessModes: [ReadWriteOnce]
  resources: {requests: {storage: 2Gi}}
---
apiVersion: apps/v1
kind: Deployment
metadata: {name: ${MINIO_NAME}, namespace: ${APPS_NAMESPACE}}
spec:
  replicas: 1
  selector: {matchLabels: {app: ${MINIO_NAME}}}
  template:
    metadata: {labels: {app: ${MINIO_NAME}}}
    spec:
      containers:
        - name: minio
          image: quay.io/minio/minio:RELEASE.2025-04-22T22-12-26Z
          args: ["server", "/data", "--console-address", ":9001"]
          envFrom:
            - secretRef: {name: ${MINIO_NAME}}
          ports: [{containerPort: 9000}]
          volumeMounts: [{name: data, mountPath: /data}]
      volumes:
        - name: data
          persistentVolumeClaim: {claimName: ${MINIO_NAME}}
---
apiVersion: v1
kind: Service
metadata: {name: ${MINIO_NAME}, namespace: ${APPS_NAMESPACE}}
spec:
  selector: {app: ${MINIO_NAME}}
  ports: [{name: s3, port: 9000, targetPort: 9000}]
YAML
  MINIO_CREATED=1
  kubectl -n "$APPS_NAMESPACE" rollout status "deploy/${MINIO_NAME}" --timeout=300s >/dev/null
  MINIO_IP="$(kubectl -n "$APPS_NAMESPACE" get pod -l "app=${MINIO_NAME}" -o jsonpath='{.items[0].status.podIP}')"
  [[ -n "$MINIO_IP" ]] || die "MinIO pod has no IP"
  ENDPOINT="http://${MINIO_IP}:9000"
  say "drill-only address-policy relaxation for ${MINIO_IP}"
  ORIG_PRIVATE_CIDRS="$(kubectl -n "$NAMESPACE" get deploy/naslos-api -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="CLOUD_PRIVATE_CIDRS")].value}')"
  ORIG_BLOCKED_CIDRS="$(kubectl -n "$NAMESPACE" get deploy/naslos-api -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="CLOUD_BLOCKED_CIDRS")].value}')"
  kubectl -n "$NAMESPACE" set env deploy/naslos-api \
    CLOUD_PRIVATE_CIDRS="${MINIO_IP}/32:9000" \
    CLOUD_BLOCKED_CIDRS="" >/dev/null
  API_PATCHED=1
  cat <<YAML | kubectl apply -f - >/dev/null
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {name: naslos-api-cloud-egress-drill, namespace: ${NAMESPACE}}
spec:
  podSelector: {matchLabels: {app.kubernetes.io/name: naslos-api}}
  policyTypes: [Egress]
  egress:
    - to: [{ipBlock: {cidr: "${MINIO_IP}/32"}}]
      ports: [{protocol: TCP, port: 9000}]
YAML
  kubectl -n "$NAMESPACE" rollout status deploy/naslos-api --timeout=300s >/dev/null
fi

# Create the bucket from inside the API pod's network (a throwaway `mc` client
# would need credentials; the MinIO admin API via the bundled `mc` image is
# simpler). For an operator-provided endpoint, create it yourself.
if [[ "$MINIO_CREATED" == "1" ]]; then
  say "creating bucket ${MINIO_BUCKET}"
  kubectl -n "$APPS_NAMESPACE" run "${MINIO_NAME}-mc" --rm -i --restart=Never \
    --image=quay.io/minio/mc:RELEASE.2025-04-16T18-13-26Z \
    --command -- env "MC_HOST_drill=http://${MINIO_USER}:${MINIO_PASS}@${MINIO_NAME}:9000" \
    sh -c "mc mb --ignore-existing drill/${MINIO_BUCKET} && mc anonymous set none drill/${MINIO_BUCKET}" >/dev/null 2>&1 \
    || die "could not create the MinIO bucket (mc job failed)"
fi

say "creating destination"
DEST="$(api_ok POST /api/cloud/destinations "$(jq -nc \
  --arg name "drill" --arg endpoint "$ENDPOINT" --arg bucket "$MINIO_BUCKET" \
  --arg prefix "$DRILL_PREFIX" --arg ak "$MINIO_ACCESS" --arg sk "$MINIO_SECRET" \
  '{name:$name,endpoint:$endpoint,region:"us-east-1",bucket:$bucket,prefix:$prefix,pathStyle:true,allowInsecureHttp:true,enabled:true,accessKeyId:$ak,secretAccessKey:$sk}')")"
DEST_ID="$(jq -r '.destination.id' <<<"$DEST")"
[[ -n "$DEST_ID" && "$DEST_ID" != "null" ]] || die "destination create returned no id: $DEST"
TEST="$(api_ok POST "/api/cloud/destinations/${DEST_ID}/test")"
jq -e '.ok == true' >/dev/null <<<"$TEST" || die "destination test failed: $TEST"
say "destination ${DEST_ID} OK (conditionalCreate=$(jq -r '.conditionalCreate' <<<"$TEST"))"

say "creating throwaway dataset ${SOURCE_DATASET}"
api_ok POST /api/datasets "$(jq -nc --arg pool "$POOL" --arg name "$DRILL_SOURCE_NAME" '{pool:$pool,name:$name}')" >/dev/null
kubectl -n "$PRIV_NAMESPACE" exec "deploy/${TERMINAL_DEPLOY}" -- sh -c \
  "mkdir -p /var/mnt/${SOURCE_DATASET} && dd if=/dev/urandom of=/var/mnt/${SOURCE_DATASET}/payload.bin bs=1M count=${DRILL_SIZE_MB} status=none && echo drill > /var/mnt/${SOURCE_DATASET}/marker.txt" \
  || die "could not write the drill payload (does the terminal pod mount /var/mnt?)"

say "first send"
JOB="$(api_ok POST /api/cloud/send "$(jq -nc --arg d "$SOURCE_DATASET" --arg dest "$DEST_ID" '{dataset:$d,destination:$dest,keepLineages:2}')" | jq -r '.jobId')"
FIRST="$(wait_job "$JOB")"
jq -e '.state == "succeeded"' >/dev/null <<<"$FIRST" || die "first send did not succeed: $FIRST"

say "second send with an interruption"
# Make the incremental large enough to interrupt mid-stream.
kubectl -n "$PRIV_NAMESPACE" exec "deploy/${TERMINAL_DEPLOY}" -- sh -c \
  "dd if=/dev/urandom of=/var/mnt/${SOURCE_DATASET}/payload.bin bs=1M count=${DRILL_SIZE_MB} status=none && echo interrupt-me >> /var/mnt/${SOURCE_DATASET}/marker.txt"
JOB="$(api_ok POST /api/cloud/send "$(jq -nc --arg d "$SOURCE_DATASET" --arg dest "$DEST_ID" '{dataset:$d,destination:$dest,keepLineages:2}')" | jq -r '.jobId')"
# Wait until at least one chunk is uploaded, then kill the API mid-stream. The
# resume state and ledger are on the state PVC; the published manifest is not.
for _ in $(seq 1 120); do
  UPLOADED="$(api_ok GET "/api/cloud/jobs/${JOB}" | jq -r '.progress.uploaded // 0')"
  [[ "$UPLOADED" -gt 0 ]] && break
  sleep 1
done
say "deleting the API pod to interrupt the send"
kubectl -n "$NAMESPACE" delete pod -l app.kubernetes.io/name=naslos-api --wait=false >/dev/null
kubectl -n "$NAMESPACE" rollout status deploy/naslos-api --timeout=300s >/dev/null

say "resuming the send"
RESUMED_JOB="$(api_ok POST /api/cloud/send "$(jq -nc --arg d "$SOURCE_DATASET" --arg dest "$DEST_ID" '{dataset:$d,destination:$dest,keepLineages:2}')" | jq -r '.jobId')"
RESUMED="$(wait_job "$RESUMED_JOB")"
SKIPPED="$(jq -r '.progress.skipped // 0' <<<"$RESUMED")"
say "resumed send finished with skipped=${SKIPPED} (a resumed link may legitimately re-upload the tail)"
CHAIN="$(jq -r '.result.chain' <<<"$RESUMED")"
[[ -n "$CHAIN" && "$CHAIN" != "null" ]] || die "resumed send returned no chain: $RESUMED"

say "chains"
api_ok GET "/api/cloud/chains?destination=${DEST_ID}&source=$(jq -rn --arg s "$SOURCE_DATASET" '$s|@uri')" | jq -e --arg c "$CHAIN" '.chains | map(.chain) | index($c) != null' >/dev/null \
  || die "chain ${CHAIN} is not listed"

say "restoring into ${RESTORE_DATASET}"
RJOB="$(api_ok POST /api/cloud/restore "$(jq -nc --arg d "$RESTORE_DATASET" --arg s "$SOURCE_DATASET" --arg dest "$DEST_ID" --arg c "$CHAIN" '{destination:$dest,source:$s,chain:$c,dataset:$d}')" | jq -r '.jobId')"
RESTORED="$(wait_job "$RJOB")"
jq -e '.state == "succeeded"' >/dev/null <<<"$RESTORED" || die "restore failed: $RESTORED"

say "comparing restored content"
SRC_HASH="$(kubectl -n "$PRIV_NAMESPACE" exec "deploy/${TERMINAL_DEPLOY}" -- sha256sum "/var/mnt/${SOURCE_DATASET}/payload.bin" | awk '{print $1}')"
DST_HASH="$(kubectl -n "$PRIV_NAMESPACE" exec "deploy/${TERMINAL_DEPLOY}" -- sha256sum "/var/mnt/${RESTORE_DATASET}/payload.bin" | awk '{print $1}')"
[[ "$SRC_HASH" == "$DST_HASH" ]] || die "restored payload differs (${SRC_HASH} != ${DST_HASH})"

say "verify structure"
VJOB="$(api_ok POST /api/cloud/verify "$(jq -nc --arg s "$SOURCE_DATASET" --arg dest "$DEST_ID" --arg c "$CHAIN" '{destination:$dest,source:$s,chain:$c,mode:"structure"}')" | jq -r '.jobId')"
wait_job "$VJOB" >/dev/null
say "verify content"
VJOB="$(api_ok POST /api/cloud/verify "$(jq -nc --arg s "$SOURCE_DATASET" --arg dest "$DEST_ID" --arg c "$CHAIN" '{destination:$dest,source:$s,chain:$c,mode:"content"}')" | jq -r '.jobId')"
wait_job "$VJOB" >/dev/null

say "credential-leak grep"
LEAK=0
api_ok GET /api/cloud/destinations > "$TMP/destinations.json"
api_ok GET /api/cloud/jobs > "$TMP/jobs.json"
api_ok GET /api/cloud/chains?destination="${DEST_ID}"\&source="$(jq -rn --arg s "$SOURCE_DATASET" '$s|@uri')" > "$TMP/chains.json"
for file in "$TMP"/*.json; do
  if grep -qF -e "$MINIO_ACCESS" -e "$MINIO_SECRET" -e "naslos-cloud-cred" "$file"; then
    echo "LEAK in $file" >&2
    LEAK=1
  fi
done
kubectl -n "$NAMESPACE" logs deploy/naslos-api --tail=5000 2>/dev/null > "$TMP/api.log" || true
if grep -qF -e "$MINIO_ACCESS" -e "$MINIO_SECRET" "$TMP/api.log"; then
  echo "LEAK in the API log" >&2
  LEAK=1
fi
[[ "$LEAK" == "0" ]] || die "credential material appeared in an API response or log"
say "no credentials or Secret names in the captured responses/logs"

say "PASS"
if [[ "$KEEP" != "1" ]]; then
  say "cleaning up the throwaway datasets/destination/MinIO (set KEEP=1 to keep them)"
fi
