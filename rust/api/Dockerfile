# Naslos API server container image (Rust port).
#
# Build context: repo root. Unlike the Go API (which embedded the Helm and Talos
# SDKs), the Rust port drives the bundled CLIs, so this image ships pinned
# `helm`, `kubectl` and `talosctl` plus `git` for the chart-repository cache.
# That adds image size, not idle RSS.

FROM rust:1.88-alpine AS builder
RUN apk add --no-cache musl-dev

WORKDIR /build
COPY rust/Cargo.toml Cargo.toml
COPY rust/Cargo.lock Cargo.lock
COPY rust/agent/Cargo.toml agent/Cargo.toml
COPY rust/api/Cargo.toml api/Cargo.toml
# Warm the dependency cache before copying sources (placeholder crates, then
# touched after the real COPY so they rebuild).
RUN mkdir -p agent/src api/src \
    && echo 'pub fn placeholder(){}' > agent/src/lib.rs && echo 'fn main(){}' > agent/src/main.rs \
    && echo 'pub fn placeholder(){}' > api/src/lib.rs && echo 'fn main(){}' > api/src/main.rs \
    && cargo build --release --package naslos-api || true
COPY rust/agent/src agent/src
COPY rust/api/src api/src
RUN touch agent/src/lib.rs agent/src/main.rs api/src/lib.rs api/src/main.rs \
    && cargo build --release --package naslos-api

FROM alpine:3.24

# Pinned to the versions the repo runs against (node Talos v1.14.1, K8s v1.37,
# Helm v4.3). `git` powers the chart-repository cache; `openssh-client` is for
# SSH-authenticated sources.
ARG KUBECTL_VERSION=v1.37.0
ARG HELM_VERSION=v4.3.0
ARG TALOSCTL_VERSION=v1.14.1

RUN apk add --no-cache ca-certificates git openssh-client \
    && wget -q -O /usr/local/bin/kubectl "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/amd64/kubectl" \
    && chmod +x /usr/local/bin/kubectl \
    && wget -q -O /tmp/helm.tgz "https://get.helm.sh/helm-${HELM_VERSION}-linux-amd64.tar.gz" \
    && tar -xzf /tmp/helm.tgz -C /tmp \
    && mv /tmp/linux-amd64/helm /usr/local/bin/helm \
    && rm -rf /tmp/helm.tgz /tmp/linux-amd64 \
    && wget -q -O /usr/local/bin/talosctl "https://github.com/siderolabs/talos/releases/download/${TALOSCTL_VERSION}/talosctl-linux-amd64" \
    && chmod +x /usr/local/bin/talosctl \
    && addgroup -g 65532 -S naslos \
    && adduser -u 65532 -S -G naslos -h /home/naslos naslos \
    && mkdir -p /home/naslos \
    && chown -R 65532:65532 /home/naslos

WORKDIR /app
COPY --from=builder /build/target/release/naslos-api /app/naslos-api

ENV HOME=/home/naslos
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/app/naslos-api"]
