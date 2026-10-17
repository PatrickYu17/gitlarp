"use client";

import { useEffect, useState } from "react";

export type ScheduleSpec = {
  min?: number;
  max?: number;
  weekends?: boolean;
  catchup?: number;
  from?: string | null;
  to?: string | null;
};

export type Schedule = {
  id: string;
  broken?: boolean;
  spec?: ScheduleSpec;
  lastRun?: string;
  createdAt?: string;
};

const DISABLED_MSG =
  "Scheduling is disabled on this API server — no schedule secret is configured.";
const RATE_LIMITED_MSG = "rate limited — wait a minute and try again";

// UTC everywhere, mirroring Graph.tsx: the API validates dates against UTC.
const pad2 = (n: number) => String(n).padStart(2, "0");

const fmt = (d: Date) =>
  `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}-${pad2(d.getUTCDate())}`;

/** Today's date at noon UTC, so date-only strings stay stable all day. */
const todayStr = () => {
  const d = new Date();
  d.setUTCHours(12, 0, 0, 0);
  return fmt(d);
};

const addDays = (dateStr: string, n: number) => {
  const d = new Date(dateStr + "T12:00:00Z");
  d.setUTCDate(d.getUTCDate() + n);
  return fmt(d);
};

/**
 * Days each spec will commit on, as a union: from `max(spec.from, today+1)`
 * through `min(spec.to ?? today+horizon, today+horizon)`, skipping UTC
 * weekends unless the spec asks for them. Pure — the graph overlays these
 * as ghost cells before the server ever runs.
 */
export function upcomingDays(
  specs: ScheduleSpec[],
  today: string,
  horizon = 62
): Set<string> {
  const days = new Set<string>();
  const earliest = addDays(today, 1);
  const latest = addDays(today, horizon);
  for (const spec of specs) {
    const from = spec.from && spec.from > earliest ? spec.from : earliest;
    const to = spec.to && spec.to < latest ? spec.to : latest;
    const weekends = spec.weekends ?? true;
    const d = new Date(from + "T12:00:00Z");
    const end = new Date(to + "T12:00:00Z");
    while (d.getTime() <= end.getTime()) {
      const dow = d.getUTCDay();
      if (weekends || (dow !== 0 && dow !== 6)) days.add(fmt(d));
      d.setUTCDate(d.getUTCDate() + 1);
    }
  }
  return days;
}

const noop = () => {};

export default function SchedulesPanel({
  pat,
  apiBase,
  onGhostDays = noop,
}: {
  pat: string;
  apiBase: string;
  onGhostDays?: (dates: Set<string>) => void;
}) {
  const [schedules, setSchedules] = useState<Schedule[]>([]);
  const [err, setErr] = useState("");
  const [msg, setMsg] = useState("");
  const [disabled, setDisabled] = useState(false);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [min, setMin] = useState(1);
  const [max, setMax] = useState(3);
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [catchup, setCatchup] = useState(14);
  const [weekends, setWeekends] = useState(true);

  // List fetch, debounced like the graph fetch in Graph.tsx.
  useEffect(() => {
    if (!pat) {
      setSchedules([]);
      setErr("");
      setDisabled(false);
      onGhostDays(new Set());
      setLoading(false);
      return;
    }
    let alive = true;
    setLoading(true);
    const t = setTimeout(async () => {
      try {
        const res = await fetch(`${apiBase}/api/schedules`, {
          headers: { Authorization: `Bearer ${pat}` },
        });
        let body: { schedules?: Schedule[]; error?: string } | null = null;
        try {
          body = await res.json();
        } catch {
          body = null; // non-JSON body (proxy/HTML error page); surfaced below
        }
        if (!alive) return;
        if (res.status === 401) {
          setErr("invalid PAT");
        } else if (res.status === 503) {
          setDisabled(true);
          setErr(DISABLED_MSG);
        } else if (res.status === 429) {
          setErr(RATE_LIMITED_MSG);
        } else if (!res.ok || !body || body.error) {
          setErr(body?.error ?? `failed (HTTP ${res.status})`);
        } else {
          const list = body.schedules ?? [];
          setSchedules(list);
          setDisabled(false);
          setErr("");
          onGhostDays(
            upcomingDays(
              list.filter((s) => !s.broken && s.spec).map((s) => s.spec as ScheduleSpec),
              todayStr()
            )
          );
        }
      } catch (e) {
        if (alive) setErr(e instanceof Error ? e.message : String(e));
      } finally {
        if (alive) setLoading(false);
      }
    }, 500);
    return () => {
      alive = false;
      clearTimeout(t);
    };
  }, [pat, apiBase, attempt, onGhostDays]);

  // A failed fetch says nothing about current schedules — drop them rather
  // than show stale data.
  const fail = (message: string) => {
    setSchedules([]);
    onGhostDays(new Set());
    setErr(message);
  };

  const create = async () => {
    if (busy || !pat || disabled) return;
    if (min > max) {
      setErr("min must be ≤ max");
      return;
    }
    setBusy(true);
    setMsg("");
    try {
      const res = await fetch(`${apiBase}/api/schedules`, {
        method: "POST",
        headers: { Authorization: `Bearer ${pat}`, "Content-Type": "application/json" },
        body: JSON.stringify({
          pat,
          min,
          max,
          weekends,
          catchup,
          ...(from ? { from } : {}),
          ...(to ? { to } : {}),
        }),
      });
      let body: { id?: string; error?: string } | null = null;
      try {
        body = await res.json();
      } catch {
        body = null;
      }
      if (res.status === 401) fail("invalid PAT");
      else if (res.status === 503) {
        setDisabled(true);
        setErr(DISABLED_MSG);
      } else if (res.status === 429) {
        setErr(RATE_LIMITED_MSG);
      } else if (!res.ok || !body || body.error) {
        setErr(body?.error ?? `failed (HTTP ${res.status})`);
      } else {
        setErr("");
        setMsg("schedule created");
        setAttempt((a) => a + 1);
      }
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    }
    setBusy(false);
  };

  const remove = async (id: string) => {
    setBusy(true);
    setMsg("");
    try {
      const res = await fetch(`${apiBase}/api/schedules?id=${encodeURIComponent(id)}`, {
        method: "DELETE",
        headers: { Authorization: `Bearer ${pat}` },
      });
      let body: { deleted?: boolean; error?: string } | null = null;
      try {
        body = await res.json();
      } catch {
        body = null;
      }
      if (res.status === 401) fail("invalid PAT");
      else if (res.status === 429) setErr(RATE_LIMITED_MSG);
      else if (!res.ok || !body || body.error) setErr(body?.error ?? `failed (HTTP ${res.status})`);
      else {
        setErr("");
        setMsg("schedule deleted");
        setAttempt((a) => a + 1);
      }
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    }
    setBusy(false);
  };

  return (
    <section id="schedules" className="schedules">
      <h2>Schedules (auto-pilot)</h2>
      {!pat ? (
        <p className="hint">enter a PAT above to manage schedules</p>
      ) : (
        <>
          {loading && <p role="status">loading schedules…</p>}
          {err && <p role="alert">{err}</p>}
          {msg && <p>{msg}</p>}
          <ul>
            {schedules.map((s) =>
              s.broken || !s.spec ? (
                <li key={s.id}>
                  <span>{s.id} broken (undecryptable — was the secret rotated?)</span>
                </li>
              ) : (
                <li key={s.id}>
                  <span>
                    {s.spec.min}–{s.spec.max}/day •{" "}
                    {s.spec.weekends ? "weekends on" : "weekends off"} • {s.spec.from ?? "today"} →{" "}
                    {s.spec.to ?? "ongoing"} • catch-up {s.spec.catchup}
                  </span>
                  <span className="meta">last run: {s.lastRun ?? "never"}</span>
                  <button disabled={busy} onClick={() => remove(s.id)}>
                    Delete
                  </button>
                </li>
              )
            )}
          </ul>
          {!disabled && (
            <form
              className="form-row"
              onSubmit={(e) => {
                e.preventDefault();
                create();
              }}
            >
              <label htmlFor="sched-min">min</label>
              <input
                id="sched-min"
                type="number"
                min={1}
                max={100}
                value={min}
                onChange={(e) => setMin(+e.target.value)}
              />
              <label htmlFor="sched-max">max</label>
              <input
                id="sched-max"
                type="number"
                min={1}
                max={100}
                value={max}
                onChange={(e) => setMax(+e.target.value)}
              />
              <label htmlFor="sched-from">from</label>
              <input id="sched-from" type="date" value={from} onChange={(e) => setFrom(e.target.value)} />
              <label htmlFor="sched-to">to</label>
              <input id="sched-to" type="date" value={to} onChange={(e) => setTo(e.target.value)} />
              <label htmlFor="sched-catchup">catch-up</label>
              <input
                id="sched-catchup"
                type="number"
                min={1}
                max={365}
                value={catchup}
                onChange={(e) => setCatchup(+e.target.value)}
              />
              <label htmlFor="sched-weekends">
                <input
                  id="sched-weekends"
                  type="checkbox"
                  checked={weekends}
                  onChange={(e) => setWeekends(e.target.checked)}
                />
                weekends
              </label>
              <button disabled={busy}>Create schedule</button>
            </form>
          )}
        </>
      )}
    </section>
  );
}
