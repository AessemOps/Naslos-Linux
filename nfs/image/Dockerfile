# Naslos NFS server (NFS-Ganesha)
#
# Talos ships no kernel NFS server - only the nfs/nfs4 clients - so there is no
# nfsd to read /etc/exports and no rpcbind to register with. NFS-Ganesha serves
# NFSv4 entirely in userspace on a single TCP port, which needs neither.
#
# Runs as a hostNetwork DaemonSet so clients reach it on the node's :2049.

FROM debian:trixie-slim

LABEL maintainer="Naslos <team@naslos.dev>"
LABEL description="NFS-Ganesha userspace NFSv4 server for Naslos"

# dist-upgrade pulls the current Debian security pocket over the base image.
# trixie-slim ships whatever versions were current when the tag was pushed, and
# the trivy scan found base-library HIGHs (util-linux, systemd, ncurses, perl,
# expat) that Debian marks as having no fix yet; this step keeps a rebuild
# current the moment the security pocket publishes one, rather than pinning the
# base lag.
RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get dist-upgrade -y && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        nfs-ganesha \
        nfs-ganesha-vfs \
        nfs-common \
        procps \
        tini \
        && rm -rf /var/lib/apt/lists/*

# The VFS FSAL enumerates mounted filesystems through /etc/mtab, which Debian
# containers do not ship (systemd normally creates it). Without this every
# export fails to load with:
#   vfs_create_export: resolve_posix_filesystem(<path>) returned No such file or directory
RUN ln -sf /proc/mounts /etc/mtab

# The API renders ganesha.conf into the share config directory (mounted from the
# node); this is only a fallback location.
RUN mkdir -p /etc/naslos/shares /var/log/ganesha /var/run/ganesha

COPY entrypoint.sh /usr/local/bin/naslos-nfs-entrypoint
RUN chmod +x /usr/local/bin/naslos-nfs-entrypoint

# NFSv4 on TCP. NFSv3 would need rpcbind (111) and statd, which do not exist on
# Talos, so they are deliberately not exposed.
EXPOSE 2049

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/naslos-nfs-entrypoint"]
