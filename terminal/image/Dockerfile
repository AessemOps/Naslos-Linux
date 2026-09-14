# Naslos terminal image
#
# The exec target for the web terminal. Talos ships no shell of its own, so the
# terminal has to run in a container that brings its own - and that container has
# to be privileged with the host mounted, because the point of a NAS terminal is
# inspecting and repairing the host: pools, datasets, disks.
#
# ZFS tooling comes from the *host* (/host/usr/local/sbin), not from this image:
# zfsutils-linux lives in Debian's contrib component and would only duplicate the
# host's own binaries, while `chroot /host` gives the versions that actually
# match the running kernel module. A login shell gets `zpool`/`zfs` wrappers for
# it (see /etc/profile.d), so `zpool status` works as typed.
#
# This is intentionally a powerful container: it is gated by the UI's auth
# (Authelia forwardAuth) and documented as such in docs/terminal.md.

FROM debian:bookworm-slim

LABEL maintainer="Naslos <team@naslos.dev>"
LABEL description="Administrative shell for the Naslos web terminal"

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        bash \
        e2fsprogs \
        util-linux \
        procps \
        iproute2 \
        iputils-ping \
        dnsutils \
        curl \
        ca-certificates \
        less \
        nano \
        jq \
        tini \
        && rm -rf /var/lib/apt/lists/*

# /host is the Talos host filesystem, /var/mnt the datasets, /dev the devices.
RUN mkdir -p /host /var/mnt

# Make the host's ZFS tooling available under the names people expect. Defined
# in profile.d because the terminal starts a login shell (`bash -l`).
COPY naslos-shell.sh /etc/profile.d/naslos-shell.sh
RUN chmod 0644 /etc/profile.d/naslos-shell.sh

# The pod stays alive so exec sessions can attach to it; the shell is started by
# the terminal (`bash -l` by default), not by the container.
CMD ["sleep", "infinity"]

