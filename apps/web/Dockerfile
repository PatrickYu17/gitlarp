# gitlarp web UI: one-click Docker deploy.
# Build context is the repo root (see docker-compose.yml).

ARG NEXT_PUBLIC_GITLARP_API_URL

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
EXPOSE 3000
CMD ["bun", "run", "start"]
