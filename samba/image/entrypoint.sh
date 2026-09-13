#!/bin/bash
# Naslos Samba entrypoint.
#
# Runs smbd against the API-rendered smb.conf and reloads it in place when the
# file changes. The config is never copied: smbd reads it straight from the
# shared directory so that a reload always picks up exactly what the API wrote.
#
# A reload is only attempted after `testparm` accepts the new file, so a bad
# render leaves the running server on its previous, working configuration
# instead of taking SMB down.
#
# SMB_CONF_PATH is exported for every Samba tool invoked here (pdbedit,
# testparm, smbcontrol). This is essential: without it those tools default to
# /etc/samba/smb.conf and therefore read/write a *different* passdb
# (/var/lib/samba/private/passdb.tdb) than the running smbd, so synced accounts
# silently never take effect and every login falls back to guest.
set -uo pipefail

CONF="${SMB_CONF:-/etc/naslos/shares/smb.conf}"
export SMB_CONF_PATH="$CONF"
CONF_DIR="$(dirname "$CONF")"

# Account file rendered by the API: smbpasswd format, NT hashes synced from
# LDAP password changes (see api/internal/shares/smbusers.go).
USERS_FILE="${SMB_USERS_FILE:-$CONF_DIR/smbusers}"

CHECK_INTERVAL="${CONF_CHECK_INTERVAL:-3}"
WAIT_RETRIES="${CONF_WAIT_RETRIES:-30}"

log() { echo "[naslos-samba] $*"; }

mkdir -p "$CONF_DIR/private" "$CONF_DIR/lock" "$CONF_DIR/state" "$CONF_DIR/cache" /var/log/samba

mtime() { stat -c %Y "$1" 2>/dev/null || echo 0; }

# import_users merges the API-rendered account file into the passdb smbd
# actually uses. `pdbedit -i smbpasswd:` creates or updates each account
# in place, including its NT hash, so LDAP password changes propagate without
# anyone running interactive tooling on the node.
import_users() {
  [ -s "$USERS_FILE" ] || return 0
  if pdbedit -i "smbpasswd:$USERS_FILE" >/tmp/pdbedit-import.out 2>&1; then
    log "imported SMB accounts from $USERS_FILE"
  else
    log "WARN: importing $USERS_FILE failed:"
    sed 's/^/  /' /tmp/pdbedit-import.out | tail -5
  fi
}

# Best-effort wait for the first render: if it never arrives we still start
# smbd with a minimal config, because an unreachable SMB service is worse than
# an empty one (and the API pushes the real config moments later).
i=0
while [ ! -s "$CONF" ] && [ "$i" -lt "$WAIT_RETRIES" ]; do
  i=$((i + 1))
  log "waiting for $CONF ... ($i/$WAIT_RETRIES)"
  sleep 1
done

if [ ! -s "$CONF" ]; then
  log "WARN: no configuration at $CONF - starting with a minimal global section"
  mkdir -p "$CONF_DIR"
  cat > "$CONF" <<'EOF'
[global]
   workgroup = NASLOS
   server string = Naslos
   security = user
   log file = /var/log/samba/%m.log
EOF
fi

if ! testparm -s "$CONF" >/dev/null 2>&1; then
  log "ERROR: $CONF is not a valid smb.conf:"
  testparm -s "$CONF" 2>&1 | sed 's/^/  /'
  log "refusing to start with an invalid configuration"
  exit 1
fi

# Sync accounts before accepting logins.
import_users

CONF_MTIME="$(mtime "$CONF")"
USERS_MTIME="$(mtime "$USERS_FILE")"
log "starting smbd with $CONF"

smbd --foreground --no-process-group -s "$CONF" &
SMBD_PID=$!

# Reload smbd whenever the rendered config changes, and re-import accounts
# whenever the API syncs a password change.
(
  while kill -0 "$SMBD_PID" 2>/dev/null; do
    sleep "$CHECK_INTERVAL"

    CUR_USERS_MTIME="$(mtime "$USERS_FILE")"
    if [ "$CUR_USERS_MTIME" != "$USERS_MTIME" ]; then
      USERS_MTIME="$CUR_USERS_MTIME"
      import_users
    fi

    CUR_MTIME="$(mtime "$CONF")"
    [ "$CUR_MTIME" = "$CONF_MTIME" ] && continue
    CONF_MTIME="$CUR_MTIME"

    if testparm -s "$CONF" >/dev/null 2>&1; then
      log "configuration changed - reloading smbd"
      smbcontrol_out="$(smbcontrol smbd reload-config 2>&1)" || true
      [ -n "$smbcontrol_out" ] && log "$smbcontrol_out"
    else
      log "ERROR: new configuration is invalid - keeping the running configuration"
      testparm -s "$CONF" 2>&1 | sed 's/^/  /'
    fi
  done
) &
WATCHER_PID=$!

# Propagate either process exiting to the container.
wait -n "$SMBD_PID" "$WATCHER_PID"
STATUS=$?
log "shutting down (exit status $STATUS)"
kill "$SMBD_PID" "$WATCHER_PID" 2>/dev/null || true
exit "$STATUS"
