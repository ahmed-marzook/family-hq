# Two-stage build, like the main Dockerfile: Node builds the app into plain
# files, then a small Caddy image serves them. No Node in the final image.

# ---- Stage 1: build ----
FROM node:24-alpine AS builder
WORKDIR /app

# The lockfile first, so changing a page doesn't reinstall every package.
COPY package.json package-lock.json ./
RUN npm ci

COPY . .
RUN npm run build

# ---- Stage 2: serve ----
FROM caddy:2.11.7-alpine

LABEL org.opencontainers.image.title="family-hq-admin" \
      org.opencontainers.image.description="Family HQ's admin dashboard"

COPY Caddyfile /etc/caddy/Caddyfile
COPY --from=builder /app/dist /srv

# Caddy doesn't need root for a port above 1024. These are its own folders.
RUN chown -R 1000:1000 /config /data
USER 1000:1000

EXPOSE 8080

# `--spider` asks for the page without saving it. Alpine's wget is built in.
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD ["wget", "-q", "--spider", "http://127.0.0.1:8080/"]
