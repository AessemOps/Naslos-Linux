# naslos-vmd: the Naslos QEMU/KVM supervisor.
#
# Runtime: Debian bookworm-slim, because the distro ships the exact firmware
# layout the code detects (ovmf at /usr/share/ovmf, split CODE/VARS images) and
# qemu packaging maturity beat Alpine's here (CREDITS.md has the licenses and
# source links: QEMU GPLv2, OVMF BSD, SeaBIOS LGPLv3, genisoimage GPLv3).
#
# The QEMU binaries are unmodified distro packages; Naslos only orchestrates
# them, so the GPL does not extend to the Naslos sources (AGPL) or images.

# The builder MUST match the runtime distro's glibc: `rust:1.100-slim` tracks
# Debian 13 (glibc 2.41) while the runtime below is Debian 12 (glibc 2.36), and
# a dynamically linked binary built on 13 fails on 12 with
# "version `GLIBC_2.38' not found". Pin the bookworm variant so the binary only
# requires the runtime's glibc.
FROM rust:1.100-slim-bookworm@sha256:bb875b0f8ce726053e259034848c471840e96bfe591fd52556178ae9a05e6126 AS builder

WORKDIR /build
COPY rust/Cargo.toml Cargo.toml
COPY rust/Cargo.lock Cargo.lock
COPY rust/vmd/Cargo.toml vmd/Cargo.toml
# The workspace needs every member manifest to load, even when only building
# the vmd crate.
COPY rust/agent/Cargo.toml agent/Cargo.toml
COPY rust/api/Cargo.toml api/Cargo.toml
# Warm the dependency cache before copying sources so a code change does not
# rebuild the whole graph.
RUN mkdir -p vmd/src agent/src api/src \
    && echo 'fn main() {}' > vmd/src/main.rs \
    && echo 'pub fn placeholder() {}' > vmd/src/lib.rs \
    && echo 'fn main() {}' > agent/src/main.rs \
    && echo 'pub fn placeholder() {}' > agent/src/lib.rs \
    && echo 'pub fn placeholder() {}' > api/src/lib.rs \
    && echo 'fn main() {}' > api/src/main.rs \
    && cargo build --release --locked --package naslos-vmd || true
COPY rust/vmd/src vmd/src
# Bust the placeholder build (touch the lib too, so the dependency crate is
# recompiled with the real sources) and produce the real binary.
RUN touch vmd/src/main.rs vmd/src/lib.rs \
    && cargo build --release --locked --package naslos-vmd \
    && strip target/release/naslos-vmd

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc

# qemu-system-x86: the hypervisor (QMP, VNC, slirp NAT with port forwards).
# qemu-utils: qemu-img for blank/cloud-image disks.
# ovmf/seabios: UEFI (default) and BIOS firmware blobs.
# genisoimage: the cloud-init NoCloud `cidata` seed.
# ca-certificates: the URL importer verifies TLS.
# dist-upgrade pulls the current Debian security pocket over the pinned base
# image (same as the samba/nfs/openldap images), so a fixed base-library HIGH
# such as linux-pam CVE-2025-6020 does not fail the trivy gate until the next
# base digest bump.
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get dist-upgrade -y \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        qemu-system-x86 \
        qemu-utils \
        ovmf \
        seabios \
        genisoimage \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && qemu-system-x86_64 --version \
    && genisoimage --version

COPY --from=builder /build/target/release/naslos-vmd /app/naslos-vmd

# vmd is privileged by design: it opens /dev/kvm and writes VM datasets.
USER root:root

EXPOSE 9091

ENTRYPOINT ["/app/naslos-vmd"]
