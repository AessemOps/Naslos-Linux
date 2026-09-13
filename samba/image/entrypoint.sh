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

# check_nss verifies every mirrored account resolves through NSS. Samba needs a
# UNIX uid to attach a session to, so an account that exists in the passdb but
# not in NSS can never log in - it fails with NT_STATUS_ACCESS_DENIED after
# being mapped to guest, which is easy to misread as a password problem.
check_nss() {
  [ -s "$USERS_FILE" ] || return 0

  unresolved=0
  checked=0
  while IFS=: read -r name _uid _rest; do
    case "$name" in ''|'#'*) continue ;; esac
    checked=$((checked + 1))
    if ! getent passwd "$name" >/dev/null 2>&1; then
      unresolved=$((unresolved + 1))
      log "WARN: $name is in the passdb but does not resolve through NSS - its SMB login will be denied. Is the extrausers file mounted at /var/lib/extrausers?"
    fi
  done < "$USERS_FILE"

  [ "$checked" -gt 0 ] && [ "$unresolved" -eq 0 ] && \
    log "all $checked mirrored accounts resolve through NSS"
  return 0
}

# detect_lan_interface returns the interface holding the default route, read
# from /proc/net/route so no iproute2 is needed. Without this, avahi enumerates
# every interface on the node — including cni0, flannel.1 and the veth pairs —
# and advertises the server at pod-network addresses as well as the LAN one.
detect_lan_interface() {
  awk '$2 == "00000000" { print $1; exit }' /proc/net/route 2>/dev/null
}

# start_discovery advertises this host as an SMB server so it shows up when a
# client browses the network (Dolphin/Finder via mDNS, Windows via WSD). It is
# best-effort: if discovery cannot start, SMB itself keeps working, so a
# failure here is logged and never brought the server down.
start_discovery() {
  if [ "${SMB_DISCOVERY_ENABLED:-true}" != "true" ]; then
    log "network discovery disabled"
    return 0
  fi

  name="${SMB_DISCOVERY_NAME:-naslos}"
  workgroup="${SMB_WORKGROUP:-NASLOS}"

  # Advertise under the configured name; it must match the Samba NetBIOS name
  # the API renders into smb.conf, or clients see two different hosts.
  sed -i "s/^host-name=.*/host-name=${name}/" /etc/avahi/avahi-daemon.conf

  iface="${SMB_DISCOVERY_INTERFACE:-}"
  [ -z "$iface" ] && iface="$(detect_lan_interface)"
  if [ -n "$iface" ]; then
    sed -i "s|^allow-interfaces=.*|allow-interfaces=${iface}|" /etc/avahi/avahi-daemon.conf
    log "restricting discovery to interface $iface"
  fi

  mkdir -p /run/dbus /run/avahi-daemon
  chown avahi:avahi /run/avahi-daemon 2>/dev/null || true

  # avahi-daemon needs the system D-Bus.
  if ! dbus-daemon --system --fork >/tmp/dbus.out 2>&1; then
    log "WARN: could not start dbus - network discovery unavailable"
    sed 's/^/  /' /tmp/dbus.out 2>/dev/null | tail -3
    return 0
  fi

  # --no-chroot: there is no init system inside the container to provide it.
  if avahi-daemon --no-chroot --daemonize >/tmp/avahi.out 2>&1; then
    log "advertising _smb._tcp as ${name}.local (mDNS)"
  else
    log "WARN: avahi-daemon failed to start - mDNS discovery unavailable"
    sed 's/^/  /' /tmp/avahi.out 2>/dev/null | tail -5
  fi

  # Web Service Discovery: what Windows Explorer's Network view uses now that
  # SMBv1 browsing is gone.
  if command -v wsdd >/dev/null 2>&1; then
    if [ -n "$iface" ]; then
      wsdd -n "$name" -w "$workgroup" -4 -s -i "$iface" >/tmp/wsdd.out 2>&1 &
    else
      wsdd -n "$name" -w "$workgroup" -4 -s >/tmp/wsdd.out 2>&1 &
    fi
    log "advertising via WSD as $name (workgroup $workgroup)"
  fi

  return 0
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
check_nss

CONF_MTIME="$(mtime "$CONF")"
USERS_MTIME="$(mtime "$USERS_FILE")"
log "starting smbd with $CONF"

smbd --foreground --no-process-group -s "$CONF" &
SMBD_PID=$!

# Advertise the server on the network (mDNS + WSD) once smbd is on its way up.
start_discovery

# Reload smbd whenever the rendered config changes, and re-import accounts
# whenever the API syncs a password change.
(
  while kill -0 "$SMBD_PID" 2>/dev/null; do
    sleep "$CHECK_INTERVAL"

    CUR_USERS_MTIME="$(mtime "$USERS_FILE")"
    if [ "$CUR_USERS_MTIME" != "$USERS_MTIME" ]; then
      USERS_MTIME="$CUR_USERS_MTIME"
      import_users
      check_nss
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
