# Naslos API server container image
# Build context: repo root (so both api/ and any shared packages are available).

FROM golang:1.26-alpine AS builder

WORKDIR /build

# Pre-fetch dependencies
COPY api/go.mod ./
RUN go mod download

# Copy the API source and build a static binary
COPY api/ ./
RUN CGO_ENABLED=0 GOOS=linux go build -ldflags="-s -w" -o naslos-api ./cmd

# Minimal runtime image
FROM gcr.io/distroless/static-debian12:nonroot

WORKDIR /app
COPY --from=builder /build/naslos-api /app/naslos-api

# The API serves the UI static files from this path; the Helm chart mounts them.
# (The binary itself does not need write access.)
USER nonroot:nonroot

EXPOSE 8080

ENTRYPOINT ["/app/naslos-api"]