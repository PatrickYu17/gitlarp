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

export function proxy(request: NextRequest) {
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
