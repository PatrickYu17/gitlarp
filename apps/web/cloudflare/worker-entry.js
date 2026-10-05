// Cloudflare entry point: fetch passthrough for the OpenNext worker.
// `bun run deploy-web` builds .open-next/ first, then wrangler bundles
// from this file (see wrangler.jsonc "main").
// Scheduling (cron + D1) lives in the separate Rust worker at apps/worker.
import worker from "../.open-next/worker.js";

export { DOQueueHandler, DOShardedTagCache, BucketCachePurge } from "../.open-next/worker.js";

export default {
  async fetch(request, env, ctx) {
    return worker.fetch(request, env, ctx);
  },
};
