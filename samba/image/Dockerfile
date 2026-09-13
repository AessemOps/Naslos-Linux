# Naslos Samba (SMB / Time Machine) share server
#
# Runs on the Talos node as a hostNetwork DaemonSet so SMB is reachable on
# 445/139 (SMB cannot be reverse-proxied by Traefik). The share configuration
# and ZFS datasets are mounted from the host; the API pushes smb.conf through
# the privileged naslos-agent, and this container reloads smbd when it changes.

FROM debian:bookworm-slim

LABEL maintainer="Naslos <team@naslos.dev>"
LABEL description="Samba SMB/Time Machine share server for Naslos"

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        samba \
        samba-common-bin \
        samba-vfs-modules \
        smbclient \
        tini \
        && rm -rf /var/lib/apt/lists/*

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
