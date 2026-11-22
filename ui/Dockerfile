# Naslos UI container image
# Builds the SvelteKit static site and serves it with nginx.

# Node 22 LTS on glibc: the committed lockfile was generated on glibc, so the
# build uses `npm ci` against exactly the audited dependency set (SEC-2026-G4)
# instead of resolving caret ranges inside the image. The output is static, so
# the glibc/musl distinction of the final nginx stage does not matter.
FROM node:24-bookworm-slim@sha256:dc26ae92c9fb15cf6b1d63e20f0944b2efe0f115b20e00be48fb23fceb061c54 AS builder

WORKDIR /build

COPY ui/package.json ui/package-lock.json ./
RUN npm ci --no-audit --no-fund

# Copy the UI source and build the static site
COPY ui/ ./
RUN npm run build

# Serve the static build with nginx, unprivileged (AUDIT-L2).
#
# nginxinc/nginx-unprivileged runs as uid 101, listens on 8080 and keeps its
# pid/temp files under /tmp, so the container needs no root and no writable
# rootfs. The Service maps its port 80 to this 8080.
# The `alpine-slim` variant drops the unused dynamic modules (xslt, geoip,
# image-filter, njs); this config only needs core nginx (proxy, map, try_files).
FROM nginxinc/nginx-unprivileged:1.31.7-alpine-slim@sha256:a8cb0f33d51fa107c96fd2bd3a71aaf564ec131e9d2bdf8700944f0ed8b170d7

# Copy the built static assets to nginx's document root
COPY --from=builder /build/dist /usr/share/nginx/html

# Copy the SPA nginx configuration
COPY ui/nginx.conf /etc/nginx/conf.d/default.conf

EXPOSE 8080

CMD ["nginx", "-g", "daemon off;"]