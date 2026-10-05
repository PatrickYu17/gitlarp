import { NextRequest, NextResponse } from "next/server";

const BASE_CSP = [
  "default-src 'self'",
  // 'unsafe-inline' is required for script-src: Next.js injects inline
  // bootstrap / hydration scripts on this setup, so the app cannot function
  // without them. Retained deliberately; tighten only with a nonce-based CSP.
  "script-src 'self' 'unsafe-inline'",
  // The API base is user-configurable at runtime (API URL input, widget ?api=
  // param, sessionStorage override, or NEXT_PUBLIC_GITLARP_API_URL), so the CSP
  // cannot enumerate hosts; allow arbitrary http(s) API bases.
  "connect-src 'self' http: https:",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data:",
  "base-uri 'self'",
  "object-src 'none'",
  "form-action 'self'",
];

// The widget is meant to be embedded anywhere; the main app is not.
const frameAncestors = (path: string) =>
  path === "/widget" ? "frame-ancestors *" : "frame-ancestors 'self'";

// Deliberately the legacy `middleware.ts` convention, not Next 16's
// `proxy.ts`: proxy.ts is forced onto the Node.js runtime by the
// compiler, and @opennextjs/cloudflare (1.20.x) only supports Edge
// middleware — a proxy.ts app fails its build with "Node.js
// middleware is not currently supported". The body is fully
// edge-compatible (headers only). Rename back to proxy.ts when the
// adapter ships Node middleware support
// (opennextjs/opennextjs-cloudflare#617).

export function middleware(request: NextRequest) {
  return NextResponse.next({
    headers: {
      "Content-Security-Policy": [
        ...BASE_CSP,
        frameAncestors(request.nextUrl.pathname),
      ].join("; "),
      "X-Content-Type-Options": "nosniff",
    },
  });
}
