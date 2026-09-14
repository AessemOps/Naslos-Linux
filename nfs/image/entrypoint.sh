#!/bin/bash
# Naslos NFS-Ganesha entrypoint.
#
# Serves the API-rendered ganesha.conf and reloads in place when it changes.
# Ganesha reloads its export table on SIGHUP without stopping the daemon, so
# changing a share does not interrupt clients that are already mounted - and a
# file that fails to parse leaves the running exports untouched (which is why no
# trial start is attempted: a second ganesha.nfsd would fight for port 2049).
set -uo pipefail

CONF="${GANESHA_CONF:-/etc/naslos/shares/ganesha.conf}"
CHECK_INTERVAL="${CONF_CHECK_INTERVAL:-3}"
WAIT_RETRIES="${CONF_WAIT_RETRIES:-30}"
PID_FILE="${GANESHA_PID_FILE:-/var/run/ganesha/ganesha.pid}"
LOG_FILE="${GANESHA_LOG_FILE:-/var/log/ganesha/ganesha.log}"

log() { echo "[naslos-nfs] $*"; }

mkdir -p /etc/naslos/shares /var/log/ganesha /var/run/ganesha

content_hash() {
  [ -f "$1" ] || { echo ""; return; }
  sha256sum "$1" 2>/dev/null | awk '{print $1}'
}

# Best-effort wait for the first render, matching the samba container: start
# anyway if it never arrives rather than looping forever.
i=0
while [ ! -s "$CONF" ] && [ "$i" -lt "$WAIT_RETRIES" ]; do
  i=$((i + 1))
  log "waiting for $CONF ... ($i/$WAIT_RETRIES)"
  sleep 1
done

if [ ! -s "$CONF" ]; then
  log "WARN: no configuration at $CONF - starting with an empty export table"
  mkdir -p "$(dirname "$CONF")"
  cat > "$CONF" <<'EOF'
# Naslos NFS - no shares configured yet.
NFS_CORE_PARAM {
    Protocols = 4;
    Enable_NLM = false;
    Enable_RQUOTA = false;
    NFS_Port = 2049;
    mount_path_pseudo = true;
}
EOF
fi

# check_export_paths reports whether each export is on a mounted filesystem. An
# export whose path is only a directory inside /var/mnt is served from the
# node's ephemeral partition: no snapshots, no redundancy, lost on upgrade.
check_export_paths() {
  [ -s "$CONF" ] || return 0

  mounts="$(awk '{print $2}' /proc/mounts 2>/dev/null | sort -r)"
  paths="$(sed -n 's/^[[:space:]]*Path[[:space:]]*=[[:space:]]*//p' "$CONF" | tr -d '; \r' | sort -u)"
  [ -n "$paths" ] || return 0

  # Read from a here-string rather than a pipe so the loop stays in this shell.
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    backed=""
    for m in $mounts; do
      case "$path" in
        "$m"|"$m"/*) backed="$m"; break ;;
      esac
    done
    if [ -n "$backed" ]; then
      log "export path $path is on mounted filesystem $backed"
    else
      log "WARN: export path $path is NOT on a mounted filesystem - its data can be lost. Put it on a ZFS dataset (see docs/shares.md)."
    fi
  done <<< "$paths"

  return 0
}

log "starting ganesha.nfsd with $CONF"
ganesha.nfsd -f "$CONF" -F -p "$PID_FILE" -L "$LOG_FILE" &
NFSD_PID=$!

sleep 3
if ! kill -0 "$NFSD_PID" 2>/dev/null; then
  log "ERROR: ganesha.nfsd failed to start:"
  sed 's/^/  /' "$LOG_FILE" 2>/dev/null | tail -10
  exit 1
fi
log "ganesha.nfsd running (pid $NFSD_PID), NFSv4 on :2049"

check_export_paths

# Reload exports whenever the rendered config changes.
(
  LAST_HASH="$(content_hash "$CONF")"
  while kill -0 "$NFSD_PID" 2>/dev/null; do
    sleep "$CHECK_INTERVAL"
    CUR_HASH="$(content_hash "$CONF")"
    [ "$CUR_HASH" = "$LAST_HASH" ] && continue

    # Advance the hash first: a config that fails to reload must not be retried
    # in a tight loop, and the next real change will be picked up regardless.
    LAST_HASH="$CUR_HASH"
    log "configuration changed - reloading exports (SIGHUP)"
    kill -HUP "$NFSD_PID" 2>/dev/null || log "WARN: could not signal ganesha.nfsd"

    # Surface reload problems instead of leaving them buried in the log.
    if tail -40 "$LOG_FILE" 2>/dev/null | grep -qiE 'error|invalid|fail'; then
      log "WARN: ganesha reported problems while reloading:"
      tail -5 "$LOG_FILE" 2>/dev/null | sed 's/^/  /'
    fi
    check_export_paths
  done
) &
WATCHER_PID=$!

wait -n "$NFSD_PID" "$WATCHER_PID"
STATUS=$?
log "shutting down (exit status $STATUS)"
kill "$NFSD_PID" "$WATCHER_PID" 2>/dev/null || true
exit "$STATUS"
