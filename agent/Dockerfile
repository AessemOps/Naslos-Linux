# Naslos agent container image
# Runs as a privileged DaemonSet and executes zpool/zfs via chroot /host.

FROM golang:1.26-alpine AS builder

WORKDIR /build

COPY agent/go.mod ./
RUN go mod download

COPY agent/ ./
RUN CGO_ENABLED=0 GOOS=linux go build -ldflags="-s -w" -o naslos-agent ./cmd

# Alpine (not distroless): the agent calls `chroot /host <bin>` to run
# zpool/zfs/wipefs on the Talos host, and distroless ships no chroot binary.
# Alpine's busybox provides chroot and the static Go binary runs unchanged.
FROM alpine:3.21

WORKDIR /app
COPY --from=builder /build/naslos-agent /app/naslos-agent

# The agent is privileged and must run as root to chroot /host.
USER root:root

EXPOSE 9090

ENTRYPOINT ["/app/naslos-agent"]