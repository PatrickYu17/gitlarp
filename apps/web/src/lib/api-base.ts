/**
 * API base resolution: `param` → sessionStorage → build-time env → localhost.
 * No route feeds `param` anymore: the /widget page ignores `?api=` (any site
 * can frame the widget, and a forwarded base would aim the visitor's PAT at
 * a host of the framer's choosing), and the main page reads no params.
 * The source is kept for direct lib callers.
 */

export const DEFAULT_API_BASE = "http://localhost:8080";
export const SESSION_KEY = "gitlarp_api_url";

export type ApiBaseSources = {
  /** URL `?api=` param; not wired to any route (see the header note) */
  param?: string | null;
  /** `sessionStorage.gitlarp_api_url` (the API URL input) */
  session?: string | null;
  /** build-time `NEXT_PUBLIC_GITLARP_API_URL` */
  env?: string | null;
};

function plausibleHttpUrl(v: unknown): v is string {
  if (typeof v !== "string" || v.length === 0) return false;
  try {
    const u = new URL(v);
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}

/** First plausible http(s) URL wins, in the exact order param → session → env → default. */
export function resolveApiBase(s: ApiBaseSources): string {
  for (const v of [s.param, s.session, s.env]) {
    if (plausibleHttpUrl(v)) return v.replace(/\/+$/, "");
  }
  return DEFAULT_API_BASE;
}

/** Hosts where plain `http:` never leaves the machine (loopback). */
const localHosts = new Set(["localhost", "127.0.0.1"]);

/**
 * True when a base would send the PAT in cleartext: plain `http:` to
 * anything other than localhost/127.0.0.1. Unparseable input is a safe
 * false — `resolveApiBase` only ever yields a valid http(s) URL, so
 * garbage cannot be a real destination.
 */
export function isInsecureBase(url: string): boolean {
  try {
    const u = new URL(url);
    return u.protocol === "http:" && !localHosts.has(u.hostname);
  } catch {
    return false;
  }
}
