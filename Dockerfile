# Two-stage build: compile in a big image with the Rust toolchain, then copy
# only the finished binary into a small image. The final image has no
# compiler or source code, just the program.

# ---- Stage 1: build ----
FROM rust:1.98-slim-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

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

# Don't run as root inside the container.
RUN useradd --system --uid 10001 --no-create-home familyhq

COPY --from=builder /usr/local/bin/family-hq /usr/local/bin/family-hq

USER familyhq
ENV PORT=3000
EXPOSE 3000

# `docker stop` (and Ctrl+C on `docker compose up`) sends SIGTERM by default,
# but main.rs only listens for Ctrl+C, which is SIGINT. Ask Docker to send
# SIGINT instead so the graceful shutdown runs.
STOPSIGNAL SIGINT

CMD ["family-hq"]
