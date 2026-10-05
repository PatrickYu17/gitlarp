# gitlarp web UI: one-click Docker deploy.
# Build context is the repo root (see docker-compose.yml).

FROM oven/bun:1 AS build
WORKDIR /app
# Next.js shells out to `npm config get registry` when downloading the swc
# binary: oven/bun ships without npm, so provide it for the build stage.
RUN apt-get update && apt-get install -y --no-install-recommends npm \
    && rm -rf /var/lib/apt/lists/*
COPY bun.lock package.json ./
COPY apps/web/package.json apps/web/package.json
RUN bun install --frozen-lockfile
COPY apps/web apps/web
# NEXT_PUBLIC_ vars are inlined at build time, so the ARG must reach the build.
ARG NEXT_PUBLIC_GITLARP_API_URL
RUN cd apps/web && bun run build

FROM oven/bun:1
WORKDIR /app
ARG NEXT_PUBLIC_GITLARP_API_URL
ENV NODE_ENV=production
ENV NEXT_PUBLIC_GITLARP_API_URL=$NEXT_PUBLIC_GITLARP_API_URL
COPY --from=build /app/node_modules node_modules
COPY --from=build /app/package.json package.json
COPY --from=build /app/apps/web apps/web
WORKDIR /app/apps/web
# Unprivileged runtime user, mirroring apps/server/Dockerfile. /app is
# chowned because `next start` writes cache files under .next at runtime.
RUN useradd --create-home --shell /usr/sbin/nologin app \
    && chown -R app:app /app
USER app
EXPOSE 3000
# The image ships no curl/wget, so probe with bun's own fetch (docker-smoke.sh
# does the same check via `GET /` on :3000).
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD bun -e 'fetch("http://127.0.0.1:3000/").then((r) => process.exit(r.ok ? 0 : 1)).catch(() => process.exit(1))'
CMD ["bun", "run", "start"]
