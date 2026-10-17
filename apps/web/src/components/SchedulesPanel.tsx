"use client";

import { Banner } from "@cloudflare/kumo/components/banner";
import { Button } from "@cloudflare/kumo/components/button";
import { Input } from "@cloudflare/kumo/components/input";
import { LayerCard } from "@cloudflare/kumo/components/layer-card";
import { Text } from "@cloudflare/kumo/components/text";
import { useKumoToastManager } from "@cloudflare/kumo/components/toast";
import { CalendarCheckIcon, TrashIcon, WarningCircleIcon } from "@phosphor-icons/react/dist/ssr";
import { useEffect, useState } from "react";

import { fmt, prettyDay } from "../lib/graph";
import Toggle from "./Toggle";

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

// UTC everywhere, mirroring lib/graph.ts: the API validates dates against UTC.

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
 * weekends unless the spec asks for them. Pure — the panel previews these
 * before the server ever runs. (They are all after today, so the graph,
 * which ends today, has no cells for them.)
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

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

export default function SchedulesPanel({ pat, apiBase }: { pat: string; apiBase: string }) {
  const toasts = useKumoToastManager();
  const [schedules, setSchedules] = useState<Schedule[]>([]);
  const [upcoming, setUpcoming] = useState<string[]>([]);
  const [err, setErr] = useState("");
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
      setUpcoming([]);
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
          setUpcoming(
            [
              ...upcomingDays(
                list.filter((s) => !s.broken && s.spec).map((s) => s.spec as ScheduleSpec),
                todayStr()
              ),
            ].sort()
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
  }, [pat, apiBase, attempt]);

  // A failed fetch says nothing about current schedules — drop them rather
  // than show stale data.
  const fail = (message: string) => {
    setSchedules([]);
    setUpcoming([]);
    setErr(message);
  };

  const create = async () => {
    if (busy || !pat || disabled) return;
    if (min > max) {
      setErr("min must be ≤ max");
      return;
    }
    setBusy(true);
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
        toasts.add({ title: "schedule created", variant: "success" });
        setAttempt((a) => a + 1);
      }
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    }
    setBusy(false);
  };

  const remove = async (id: string) => {
    setBusy(true);
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
        toasts.add({ title: "schedule deleted", variant: "success" });
        setAttempt((a) => a + 1);
      }
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    }
    setBusy(false);
  };

  return (
    <section id="schedules">
      <LayerCard>
        <LayerCard.Secondary className="flex items-center gap-2 font-medium">
          <CalendarCheckIcon size={16} className="text-kumo-subtle" />
          <h2 className="m-0 text-base font-medium">Schedules (auto-pilot)</h2>
        </LayerCard.Secondary>
        <LayerCard.Primary className="flex flex-col gap-4">
          {!pat ? (
            <Text variant="secondary" size="sm">
              enter a PAT above to manage schedules
            </Text>
          ) : (
            <>
              {loading && (
                <span role="status" className="sr-only">
                  loading schedules…
                </span>
              )}
              {err && (
                <Banner
                  variant="error"
                  size="sm"
                  icon={<WarningCircleIcon weight="fill" />}
                  description={<span role="alert">{err}</span>}
                />
              )}

              {schedules.length > 0 ? (
                <ul className="m-0 flex list-none flex-col divide-y divide-kumo-line p-0">
                  {schedules.map((s) => (
                    <li key={s.id} className="flex flex-wrap items-center justify-between gap-2 py-2">
                      {s.broken || !s.spec ? (
                        <Text size="sm">{s.id} broken (undecryptable — was the secret rotated?)</Text>
                      ) : (
                        <div className="flex flex-col">
                          <Text size="sm">
                            {s.spec.min}–{s.spec.max}/day • {s.spec.weekends ? "weekends on" : "weekends off"} •{" "}
                            {s.spec.from ?? "today"} → {s.spec.to ?? "ongoing"} • catch-up {s.spec.catchup}
                          </Text>
                          <Text variant="secondary" size="xs">
                            last run: {s.lastRun ?? "never"}
                          </Text>
                        </div>
                      )}
                      <Button
                        variant="ghost"
                        size="sm"
                        icon={TrashIcon}
                        disabled={busy}
                        aria-label={`Delete schedule ${s.id}`}
                        onClick={() => remove(s.id)}
                      >
                        Delete
                      </Button>
                    </li>
                  ))}
                </ul>
              ) : (
                !loading &&
                !err && (
                  <Text variant="secondary" size="sm">
                    No schedules yet.
                  </Text>
                )
              )}

              {upcoming.length > 0 && (
                <Text variant="secondary" size="sm">
                  Next scheduled day: {prettyDay(upcoming[0], true)} ({plural(upcoming.length, "day")} in the next
                  two months)
                </Text>
              )}

              {!disabled && (
                <form
                  className="flex flex-col gap-3 rounded-lg bg-kumo-recessed p-3"
                  onSubmit={(e) => {
                    e.preventDefault();
                    create();
                  }}
                >
                  <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
                    <Input
                      label="Min per day"
                      type="number"
                      size="sm"
                      min={1}
                      max={100}
                      value={min}
                      onChange={(e) => setMin(+e.target.value)}
                    />
                    <Input
                      label="Max per day"
                      type="number"
                      size="sm"
                      min={1}
                      max={100}
                      value={max}
                      onChange={(e) => setMax(+e.target.value)}
                    />
                    <Input
                      label="Catch-up days"
                      type="number"
                      size="sm"
                      min={1}
                      max={365}
                      value={catchup}
                      onChange={(e) => setCatchup(+e.target.value)}
                    />
                    <Input label="From" type="date" size="sm" value={from} onChange={(e) => setFrom(e.target.value)} />
                    <Input label="To" type="date" size="sm" value={to} onChange={(e) => setTo(e.target.value)} />
                  </div>
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <Toggle label="Weekends" checked={weekends} onChange={setWeekends} />
                    <Button type="submit" variant="secondary" loading={busy} disabled={busy}>
                      Create schedule
                    </Button>
                  </div>
                </form>
              )}
            </>
          )}
        </LayerCard.Primary>
      </LayerCard>
    </section>
  );
}
