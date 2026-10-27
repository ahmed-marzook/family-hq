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

# Don't run as root inside the container.
RUN useradd --system --uid 10001 --no-create-home familyhq

# Where the SQLite file lives. Owned by familyhq so it can write there.
# Compose mounts a volume here; Docker copies this ownership into a new,
# empty named volume, so the volume is writable too.
RUN mkdir /data && chown familyhq /data

COPY --from=builder /usr/local/bin/family-hq /usr/local/bin/family-hq

USER familyhq
ENV PORT=3000
ENV DATABASE_URL=sqlite:/data/family-hq.db
EXPOSE 3000

# `docker stop` (and Ctrl+C on `docker compose up`) sends SIGTERM by default,
# but main.rs only listens for Ctrl+C, which is SIGINT. Ask Docker to send
# SIGINT instead so the graceful shutdown runs.
STOPSIGNAL SIGINT

CMD ["family-hq"]
