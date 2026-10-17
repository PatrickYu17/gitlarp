"use client";

import { useEffect, useMemo, useState } from "react";

import { SESSION_KEY, resolveApiBase } from "../lib/api-base";
import { type Counts, clampLevel, fmt } from "../lib/graph";

const PAT_KEY = "gitlarp-pat";

function loadStored(key: string): string {
  if (typeof sessionStorage === "undefined") return "";
  try {
    return sessionStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

function store(key: string, value: string) {
  try {
    sessionStorage.setItem(key, value);
  } catch {
    // storage unavailable (private mode); value just stays in memory for this tab
  }
}

/**
 * An apply or random-fill outcome. Successes are transient (a toast);
 * errors and partial failures stay on screen until the next action.
 */
export type Outcome = { ok: boolean; text: string };

export function useGraphState({
  apiParam,
  apiBase: baseOverride,
}: {
  apiParam?: string;
  apiBase?: string;
}) {
  const [pat, setPat] = useState("");
  const [apiInput, setApiInput] = useState("");
  const [ready, setReady] = useState(false);
  const [counts, setCounts] = useState<Counts>({});
  const [plan, setPlan] = useState<Counts>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [graphErr, setGraphErr] = useState("");
  const [loading, setLoading] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [jitter, setJitter] = useState(false);

  const resolvedBase = useMemo(
    () => resolveApiBase({ session: apiInput, env: process.env.NEXT_PUBLIC_GITLARP_API_URL }),
    [apiInput]
  );
  const apiBase = baseOverride ?? resolvedBase;

  useEffect(() => {
    if (apiParam) store(SESSION_KEY, apiParam);
    setPat(loadStored(PAT_KEY));
    setApiInput(loadStored(SESSION_KEY));
    setReady(true);
  }, [apiParam]);

  useEffect(() => {
    if (ready) store(PAT_KEY, pat);
  }, [pat, ready]);

  useEffect(() => {
    if (ready) store(SESSION_KEY, apiInput);
  }, [apiInput, ready]);

  useEffect(() => {
    if (!pat) {
      setCounts({});
      setGraphErr("");
      setLoading(false);
      return;
    }
    let alive = true;
    setLoading(true);
    const t = setTimeout(async () => {
      try {
        const res = await fetch(`${apiBase}/api/graph`, {
          headers: { Authorization: `Bearer ${pat}` },
        });
        let body: { counts?: Counts; error?: string } | null = null;
        try {
          body = await res.json();
        } catch {
          body = null; // non-JSON body (proxy/HTML error page); surfaced below
        }
        if (!alive) return;
        if (res.status === 401) setGraphErr("invalid PAT");
        else if (!res.ok || !body || body.error) {
          setGraphErr(body?.error ?? `graph fetch failed (HTTP ${res.status})`);
        } else {
          setCounts(body.counts ?? {});
          setGraphErr("");
        }
      } catch (e) {
        if (alive) setGraphErr(e instanceof Error ? e.message : String(e));
      } finally {
        if (alive) setLoading(false);
      }
    }, 500);
    return () => {
      alive = false;
      clearTimeout(t);
    };
  }, [pat, apiBase, attempt]);

  const retry = () => setAttempt((a) => a + 1);

  /** Plan level for one day; 0 removes it from the plan. */
  const setDay = (date: string, v: number) => {
    const next = { ...plan };
    if (v <= 0) delete next[date];
    else next[date] = v;
    setPlan(next);
  };

  /** Cell click: step the day's plan 0 → 1 → … → 4 → 0. Returns the new level. */
  const cycleDay = (date: string) => {
    const cur = plan[date] ?? 0;
    const v = cur >= 4 ? 0 : cur + 1;
    setDay(date, v);
    return v;
  };

  const apply = async (): Promise<Outcome | null> => {
    const planDays = Object.entries(plan).map(([date, count]) => ({ date, count }));
    if (!planDays.length || busy || !pat) return null;
    setBusy(true);
    setError("");
    let outcome: Outcome;
    try {
      const res = await fetch(`${apiBase}/api/commits`, {
        method: "POST",
        headers: { Authorization: `Bearer ${pat}`, "Content-Type": "application/json" },
        body: JSON.stringify({ pat, days: planDays, ...(jitter ? { jitter: true } : {}) }),
      });
      let body: {
        created?: number;
        total?: number;
        partial?: boolean;
        clamped?: number;
        error?: string;
      } | null = null;
      try {
        body = await res.json();
      } catch {
        body = null;
      }
      if (!body) {
        outcome = { ok: false, text: `apply failed (HTTP ${res.status})` };
      } else if (body.error && !body.partial) {
        outcome = { ok: false, text: body.error };
      } else {
        outcome = {
          ok: !body.partial,
          text: `created ${body.created}/${body.total} commit(s)${
            body.partial ? ` (partial failure: ${body.error})` : ""
          }${body.clamped ? ` (${body.clamped} date(s) clamped)` : ""}`,
        };
        setPlan({});
        if (!body.partial) {
          if (body.clamped) {
            // dates moved server-side (clamped into the window); a
            // refetch is the only honest picture, not an optimistic guess
            retry();
          } else {
            const next = { ...counts };
            for (const { date, count } of planDays) next[date] = (next[date] ?? 0) + count;
            setCounts(next);
          }
        }
      }
    } catch (e) {
      outcome = { ok: false, text: e instanceof Error ? e.message : String(e) };
    }
    if (!outcome.ok) setError(outcome.text);
    setBusy(false);
    return outcome;
  };

  /** DELETE /api/commits: back to gitlarp-base, or drop the larp branch. */
  const resetRepo = async (): Promise<Outcome | null> => {
    if (busy || !pat) return null;
    setBusy(true);
    setError("");
    let outcome: Outcome;
    try {
      const res = await fetch(`${apiBase}/api/commits`, {
        method: "DELETE",
        headers: { Authorization: `Bearer ${pat}` },
      });
      let body: { kind?: string; sha?: string; branch?: string; error?: string } | null = null;
      try {
        body = await res.json();
      } catch {
        body = null;
      }
      if (!res.ok || !body || body.error) {
        outcome = { ok: false, text: body?.error ?? `reset failed (HTTP ${res.status})` };
      } else {
        outcome = {
          ok: true,
          text: body.kind === "reset" ? `reset to gitlarp-base (${body.sha})` : `deleted branch ${body.branch}`,
        };
        setPlan({});
        retry();
      }
    } catch (e) {
      outcome = { ok: false, text: e instanceof Error ? e.message : String(e) };
    }
    if (!outcome.ok) setError(outcome.text);
    setBusy(false);
    return outcome;
  };

  const randomFill = (from: string, to: string, min: number, max: number) => {
    if (!from || !to) return;
    // parse at UTC noon: date-only strings are UTC days on the wire
    const d = new Date(from + "T12:00:00Z");
    const end = new Date(to + "T12:00:00Z");
    if ((end.getTime() - d.getTime()) / 86400000 + 1 > 365) {
      setError("range too big (max 365 days)");
      return;
    }
    const next = { ...plan };
    while (d <= end) {
      const key = fmt(d);
      const c = min + Math.floor(Math.random() * (max - min + 1));
      next[key] = clampLevel((next[key] ?? 0) + c);
      d.setUTCDate(d.getUTCDate() + 1);
    }
    setPlan(next);
    setError("");
  };

  const planCount = Object.values(plan).reduce((sum, c) => sum + c, 0);

  return {
    pat,
    setPat,
    apiInput,
    setApiInput,
    apiBase,
    counts,
    plan,
    planCount,
    setDay,
    cycleDay,
    clearPlan: () => setPlan({}),
    busy,
    error,
    graphErr,
    loading,
    retry,
    apply,
    resetRepo,
    randomFill,
    jitter,
    setJitter,
  };
}

export type GraphState = ReturnType<typeof useGraphState>;
