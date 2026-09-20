# Naslos Samba (SMB / Time Machine) share server
#
# Runs on the Talos node as a hostNetwork DaemonSet so SMB is reachable on
# 445/139 (SMB cannot be reverse-proxied by Traefik). The share configuration
# and ZFS datasets are mounted from the host; the API pushes smb.conf through
# the privileged naslos-agent, and this container reloads smbd when it changes.

FROM debian:trixie-slim

LABEL maintainer="Naslos <team@naslos.dev>"
LABEL description="Samba SMB/Time Machine share server for Naslos"

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        samba \
        samba-common-bin \
        samba-vfs-modules \
        smbclient \
        libnss-extrausers \
        avahi-daemon \
        avahi-utils \
        dbus \
        wsdd2 \
        tini \
        && rm -rf /var/lib/apt/lists/*

# Network discovery: Avahi publishes _smb._tcp over mDNS so file managers that
# browse the network (Dolphin, Finder) list this server under smb:// — the same
# way a NAS appliance appears. WSD (Web Service Discovery) makes Windows
# Explorer's Network view find it too (Windows dropped SMBv1 browsing). Debian 13
# dropped the Python `wsdd` package and ships `wsdd2` (the C implementation), so
# the entrypoint launches whichever is present.
COPY avahi-daemon.conf /etc/avahi/avahi-daemon.conf
COPY avahi-smb.service /etc/avahi/services/naslos-smb.service

# The extrausers NSS module lets Samba resolve the LDAP users mirrored by the
# API into /var/lib/extrausers (mounted from the node), so an SMB session maps
# to a real UNIX uid without local accounts being created anywhere. Without it
# Samba stores an unresolvable uid (4294967295) and denies every login even
# when the passdb entry and NT hash are correct.
COPY nsswitch.conf /etc/nsswitch.conf
RUN mkdir -p /var/lib/extrausers

# The rendered config, passdb and lock state live here (mounted from the host
# at /var/lib/naslos/shares).
RUN mkdir -p /etc/naslos/shares/private \
             /etc/naslos/shares/lock \
             /etc/naslos/shares/state \
             /etc/naslos/shares/cache \
             /var/log/samba

COPY entrypoint.sh /usr/local/bin/naslos-samba-entrypoint
RUN chmod +x /usr/local/bin/naslos-samba-entrypoint

# SMB over TCP. Port 137/138 (NetBIOS name service, UDP) are intentionally not
# exposed: modern clients use SMB over 445 directly.
EXPOSE 445 139

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/naslos-samba-entrypoint"]
