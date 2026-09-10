# Naslos UI container image
# Builds the SvelteKit static site and serves it with nginx.

FROM node:20-alpine AS builder

WORKDIR /build

# Install dependencies
COPY ui/package*.json ./
RUN npm ci

# Copy the UI source and build the static site
COPY ui/ ./
RUN npm run build

# Serve the static build with nginx
FROM nginx:1.27-alpine

# Copy the built static assets to nginx's document root
COPY --from=builder /build/dist /usr/share/nginx/html

# Copy the SPA nginx configuration
COPY ui/nginx.conf /etc/nginx/conf.d/default.conf

EXPOSE 80

CMD ["nginx", "-g", "daemon off;"]