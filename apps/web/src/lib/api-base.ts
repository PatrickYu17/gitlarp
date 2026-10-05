/** API base resolution: widget ?api= param → sessionStorage → build-time env → localhost. */

export const DEFAULT_API_BASE = "http://localhost:8080";
export const SESSION_KEY = "gitlarp_api_url";

export type ApiBaseSources = {
  /** widget `?api=` URL param */
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
