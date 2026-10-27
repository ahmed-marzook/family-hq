# Two-stage build: compile in a big image with the Rust toolchain, then copy
# only the finished binary into a small image. The final image has no
# compiler or source code, just the program.

# ---- Stage 1: build ----
FROM rust:1.98-slim-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
# `sqlx::migrate!()` reads these at compile time and builds them into the
# binary, so they're needed here but not in the final image.
COPY migrations ./migrations
# The `query!` macros normally check SQL against a live database while
# compiling. There's none here, so they read the checked results saved in
# .sqlx/ by `cargo sqlx prepare` instead.
COPY .sqlx ./.sqlx
ENV SQLX_OFFLINE=true

# The cache mounts keep downloaded crates and compiled dependencies between
# builds, so changing one line of our code doesn't recompile axum and tokio.
# The target dir is a cache (not part of the image), so the binary is copied
# out of it before the step ends.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked \
    && cp target/release/family-hq /usr/local/bin/family-hq

# ---- Stage 2: run ----
# Same Debian release as the builder, so the binary's system libraries match.
FROM debian:bookworm-slim

# Shown on Docker Hub and by `docker inspect`.
LABEL org.opencontainers.image.title="family-hq" \
      org.opencontainers.image.description="Family HQ: a WhatsApp bot for shared todos and shopping lists"

# CA certificates, so HTTPS calls (to the AlAdhan prayer times API) can
# check who they're talking to. The slim image ships without them.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Where the SQLite file lives (usually a mounted folder).
RUN mkdir /data

COPY --from=builder /usr/local/bin/family-hq /usr/local/bin/family-hq

# The entrypoint starts as root only long enough to make /data writable by
# PUID:PGID, then runs family-hq as that user (never as root). See the
# script for details. `--chmod` makes it executable without a RUN step.
COPY --chmod=755 docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

ENV PUID=1000
ENV PGID=1000
ENV PORT=3000
ENV DATABASE_URL=sqlite:/data/family-hq.db
EXPOSE 3000

# Lets Docker (and `docker compose ps`) show "healthy" / "unhealthy".
# The slim image has no curl, so bash opens a TCP connection itself
# (/dev/tcp is a bash feature), sends a bare HTTP request to /health and
# looks for "ok" in the answer.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/3000 && printf 'GET /health HTTP/1.0\\r\\n\\r\\n' >&3 && grep -q '\"ok\"' <&3"]

# `docker stop` (and Ctrl+C on `docker compose up`) sends SIGTERM by default,
# but main.rs only listens for Ctrl+C, which is SIGINT. Ask Docker to send
# SIGINT instead so the graceful shutdown runs.
STOPSIGNAL SIGINT

ENTRYPOINT ["docker-entrypoint.sh"]
