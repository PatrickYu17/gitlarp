# gitlarp-server: hardened actix-web API server.
#
# Build from the workspace root (the build context needs the root
# Cargo.toml/Cargo.lock plus the core and server crates):
#
#   docker build -f apps/server/Dockerfile -t gitlarp-server .
#
# Run:
#
#   docker run -p 3000:3000 \
#     -e GITLARP_SCHEDULE_SECRET=... \
#     -v gitlarp-data:/data \
#     gitlarp-server

FROM rust:1-slim AS build
WORKDIR /build
# workspace manifest + lock first, then the crates the server needs.
# (apps/cli is part of the workspace, so its manifest must be present
# for cargo to resolve members; only gitlarp-server is compiled.)
COPY Cargo.toml Cargo.lock ./
COPY apps/core apps/core
COPY apps/server apps/server
COPY apps/cli apps/cli
RUN cargo build --release --locked -p gitlarp-server

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --shell /usr/sbin/nologin app
COPY --from=build /build/target/release/gitlarp-server /usr/local/bin/gitlarp-server

# Runtime configuration is env-driven: PORT (default 3000) and
# SCHEDULE_DIR (default ./data/schedules). Point the schedule store at
# the mounted volume so encrypted records survive restarts.
ENV PORT=3000 \
    SCHEDULE_DIR=/data/schedules
RUN mkdir -p /data/schedules && chown -R app:app /data
USER app

# Encrypted schedule records live here; mount it (docker volume or
# bind) to persist them.
VOLUME ["/data"]

EXPOSE 3000
CMD ["gitlarp-server"]
