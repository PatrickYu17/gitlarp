"use client";

import { useEffect, useMemo, useState } from "react";

import { DEFAULT_API_BASE, SESSION_KEY, resolveApiBase } from "../lib/api-base";

type Counts = Record<string, number>;

const PAT_KEY = "gitlarp-pat";

// UTC everywhere: the API validates and clamps dates against UTC, so
// local-time cells would drift a day at boundaries.
const pad2 = (n: number) => String(n).padStart(2, "0");

const fmt = (d: Date) =>
  `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}-${pad2(d.getUTCDate())}`;

/** Clamp the plan's 0–4 commit levels (matches the cell shading). */
const clampLevel = (v: number) => Math.max(0, Math.min(4, Math.floor(v) || 0));

function lastYear(): string[] {
  const days: string[] = [];
  for (let i = 364; i >= 0; i--) {
    const d = new Date();
    d.setUTCHours(12, 0, 0, 0);
    d.setUTCDate(d.getUTCDate() - i);
    days.push(fmt(d));
  }
  return days;
}

const level = (n: number) =>
  n <= 0 ? "" : n === 1 ? "l1" : n === 2 ? "l2" : n === 3 ? "l3" : "l4";

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

export default function Graph({
  tools = false,
  theme = "light",
  apiParam,
}: {
  tools?: boolean;
  theme?: string;
  /** widget `?api=` param; seeds the sessionStorage override on mount */
  apiParam?: string;
}) {
  const days = useMemo(lastYear, []);
  const [pat, setPat] = useState("");
  const [apiInput, setApiInput] = useState("");
  const [ready, setReady] = useState(false);
  const [counts, setCounts] = useState<Counts>({});
  const [plan, setPlan] = useState<Counts>({});
  const [sel, setSel] = useState<string | null>(null);
  const [n, setN] = useState(1);
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [min, setMin] = useState(1);
  const [max, setMax] = useState(5);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState("");
  const [graphErr, setGraphErr] = useState("");
  const [loading, setLoading] = useState(false);
  const [attempt, setAttempt] = useState(0);

  const envBase = process.env.NEXT_PUBLIC_GITLARP_API_URL || DEFAULT_API_BASE;
  const apiBase = useMemo(
    () => resolveApiBase({ session: apiInput, env: process.env.NEXT_PUBLIC_GITLARP_API_URL }),
    [apiInput]
  );

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

  const apply = async () => {
    const planDays = Object.entries(plan).map(([date, count]) => ({ date, count }));
    if (!planDays.length || busy || !pat) return;
    setBusy(true);
    setMsg("");
    try {
      const res = await fetch(`${apiBase}/api/commits`, {
        method: "POST",
        headers: { Authorization: `Bearer ${pat}`, "Content-Type": "application/json" },
        body: JSON.stringify({ pat, days: planDays }),
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
        setMsg(`apply failed (HTTP ${res.status})`);
      } else if (body.error && !body.partial) {
        setMsg(body.error);
      } else {
        setMsg(
          `created ${body.created}/${body.total} commit(s)${
            body.partial ? ` (partial failure: ${body.error})` : ""
          }${body.clamped ? ` (${body.clamped} date(s) clamped)` : ""}`
        );
        setPlan({});
        if (!body.partial) {
          if (body.clamped) {
            // dates moved server-side (clamped into the window); a
            // refetch is the only honest picture, not an optimistic guess
            setAttempt((a) => a + 1);
          } else {
            const next = { ...counts };
            for (const { date, count } of planDays) next[date] = (next[date] ?? 0) + count;
            setCounts(next);
          }
        }
      }
    } catch (e) {
      setMsg(e instanceof Error ? e.message : String(e));
    }
    setBusy(false);
  };

  const randomFill = () => {
    if (!from || !to) return;
    // parse at UTC noon: date-only strings are UTC days on the wire
    const d = new Date(from + "T12:00:00Z");
    const end = new Date(to + "T12:00:00Z");
    if ((end.getTime() - d.getTime()) / 86400000 + 1 > 365) {
      setMsg("range too big (max 365 days)");
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
    setMsg("");
  };

  const start = new Date();
  const planCount = Object.values(plan).reduce((sum, c) => sum + c, 0);
  start.setUTCHours(12, 0, 0, 0);
  start.setUTCDate(start.getUTCDate() - 364);
  const pad = start.getUTCDay();

  return (
    <div className={`g ${theme === "dark" ? "dark" : "light"}`}>
      <div className="inputs">
        <label htmlFor="gitlarp-pat">GitHub PAT</label>
        <input
          id="gitlarp-pat"
          type="password"
          placeholder="fine-grained token, gitlarp-history only, Contents R/W, kept in this tab only"
          value={pat}
          onChange={(e) => setPat(e.target.value)}
        />
        <label htmlFor="gitlarp-api-url">API URL</label>
        <input
          id="gitlarp-api-url"
          type="text"
          placeholder={envBase}
          value={apiInput}
          onChange={(e) => setApiInput(e.target.value)}
        />
      </div>
      <div className="grid">
        {[...Array(pad)].map((_, i) => (
          <span key={`p${i}`} />
        ))}
        {days.map((d) => {
          const real = counts[d] ?? 0;
          const planned = plan[d] ?? 0;
          return (
            <button
              key={d}
              className={`cell ${level(Math.min(4, real + planned))} ${planned ? "planned" : ""} ${
                sel === d ? "sel" : ""
              }`}
              title={`${d}: ${real} real + ${planned} planned`}
              aria-label={`${d}: ${planned} commits planned`}
              onClick={() => {
                setSel(d);
                const cur = plan[d] ?? 0;
                const nextPlan = { ...plan };
                if (cur >= 4) delete nextPlan[d];
                else nextPlan[d] = cur + 1;
                setPlan(nextPlan);
                setN(nextPlan[d] ?? 0);
              }}
            />
          );
        })}
      </div>

      {loading && (
        <p role="status">loading graph…</p>
      )}
      {graphErr && (
        <p role="alert">
          {graphErr} <button onClick={() => setAttempt((a) => a + 1)}>Retry</button>
        </p>
      )}

      {sel && (
        <div className="pop">
          <b>{sel}</b>
          <span>real: {counts[sel] ?? 0}</span>
          <span>planned: {plan[sel] ?? 0}</span>
          <input
            type="number"
            min={0}
            max={4}
            aria-label={`planned commits for ${sel}`}
            value={n}
            onChange={(e) => {
              // typing "999" clamps to the 0–4 level range, matching
              // the server-validated cells
              const v = clampLevel(+e.target.value);
              setN(v);
              const next = { ...plan };
              if (v <= 0) delete next[sel];
              else next[sel] = v;
              setPlan(next);
            }}
          />
          <button aria-label="close" onClick={() => setSel(null)}>×</button>
        </div>
      )}

      <div className="tools">
        {tools && (
          <>
            <label htmlFor="gitlarp-from">from</label>
            <input id="gitlarp-from" type="date" value={from} onChange={(e) => setFrom(e.target.value)} />
            <label htmlFor="gitlarp-to">to</label>
            <input id="gitlarp-to" type="date" value={to} onChange={(e) => setTo(e.target.value)} />
            <label htmlFor="gitlarp-min">min</label>
            <input
              id="gitlarp-min"
              type="number"
              min={0}
              max={4}
              value={min}
              onChange={(e) => setMin(clampLevel(+e.target.value))}
            />
            <label htmlFor="gitlarp-max">max</label>
            <input
              id="gitlarp-max"
              type="number"
              min={0}
              max={4}
              value={max}
              onChange={(e) => setMax(clampLevel(+e.target.value))}
            />
            <button disabled={busy} onClick={randomFill}>
              Random fill (into plan)
            </button>
          </>
        )}
        <button disabled={busy || !pat || !planCount} onClick={apply}>
          Apply plan ({planCount} commit{planCount === 1 ? "" : "s"})
        </button>
        <button disabled={busy || !planCount} onClick={() => setPlan({})}>
          Clear plan
        </button>
      </div>

      <p className="hint">
        click cells to set the plan (0–4 levels), then apply once
      </p>
      {msg && <p>{msg}</p>}
    </div>
  );
}
