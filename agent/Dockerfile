# Naslos agent container image
# Runs as a privileged DaemonSet and executes zpool/zfs via chroot /host.

FROM golang:1.26-alpine AS builder

WORKDIR /build

COPY agent/go.mod ./
RUN go mod download

COPY agent/ ./
RUN CGO_ENABLED=0 GOOS=linux go build -ldflags="-s -w" -o naslos-agent ./cmd

FROM gcr.io/distroless/static-debian12

WORKDIR /app
COPY --from=builder /build/naslos-agent /app/naslos-agent

# The agent is privileged and must run as root to chroot /host.
USER root:root

EXPOSE 9090

ENTRYPOINT ["/app/naslos-agent"]