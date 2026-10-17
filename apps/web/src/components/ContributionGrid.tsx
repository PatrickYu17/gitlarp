"use client";

import { Button } from "@cloudflare/kumo/components/button";
import { Input } from "@cloudflare/kumo/components/input";
import { Popover } from "@cloudflare/kumo/components/popover";
import { Text } from "@cloudflare/kumo/components/text";
import { Tooltip, TooltipProvider } from "@cloudflare/kumo/components/tooltip";
import { XIcon } from "@phosphor-icons/react/dist/ssr";
import { useMemo, useState } from "react";

import { clampLevel, lastYear, leadingPad, monthLabels, prettyDay } from "../lib/graph";
import type { GraphState } from "./useGraphState";

// Kumo has no heatmap, so cells stay hand-rolled; colours are the
// GitHub greens from globals.css plus Kumo tokens for the empty and
// outline states.
const LEVEL_BG = [
  "bg-kumo-recessed",
  "bg-(--gl-l1)",
  "bg-(--gl-l2)",
  "bg-(--gl-l3)",
  "bg-(--gl-l4)",
];
const PLANNED = "outline-2 -outline-offset-1 outline-kumo-warning";
/** Cell (12px) + gap (3px): one week column. */
const COL = 15;

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

export default function ContributionGrid({
  counts,
  plan,
  setDay,
  cycleDay,
  loading,
}: Pick<GraphState, "counts" | "plan" | "setDay" | "cycleDay" | "loading">) {
  const days = useMemo(lastYear, []);
  const pad = useMemo(leadingPad, []);
  const months = useMemo(() => monthLabels(days, pad), [days, pad]);
  const [sel, setSel] = useState<{ date: string; el: HTMLElement } | null>(null);
  const [n, setN] = useState(1);

  return (
    <TooltipProvider delay={200}>
      <div className="overflow-x-auto pb-1">
        <div className="flex w-fit flex-col gap-1.5">
          {/* month row, offset by the weekday column (w-6 + gap-2) */}
          <div className="relative ml-8 h-4 text-xs text-kumo-subtle" aria-hidden="true">
            {months.map((m) => (
              <span key={m.col} className="absolute top-0" style={{ left: m.col * COL }}>
                {m.label}
              </span>
            ))}
          </div>
          <div className="flex gap-2">
            <div
              className="grid w-6 grid-rows-[repeat(7,12px)] gap-[3px] text-[10px] leading-3 text-kumo-subtle"
              aria-hidden="true"
            >
              <span />
              <span>Mon</span>
              <span />
              <span>Wed</span>
              <span />
              <span>Fri</span>
              <span />
            </div>
            <div
              className={[
                "grid grid-flow-col grid-rows-[repeat(7,12px)] gap-[3px] transition-opacity",
                loading ? "animate-pulse opacity-50" : "",
              ].join(" ")}
            >
              {[...Array(pad)].map((_, i) => (
                <span key={`p${i}`} />
              ))}
              {days.map((d) => {
                const real = counts[d] ?? 0;
                const planned = plan[d] ?? 0;
                return (
                  <Tooltip
                    key={d}
                    content={
                      <span>
                        <b>{plural(real, "commit")}</b> on {prettyDay(d)}
                        {planned > 0 && <span className="text-kumo-warning"> · +{planned} planned</span>}
                      </span>
                    }
                    render={
                      <button
                        type="button"
                        aria-label={`${d}: ${planned} commits planned`}
                        className={[
                          "size-3 cursor-pointer rounded-[3px] border-0 p-0 transition-transform hover:scale-125",
                          "focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-kumo-focus",
                          LEVEL_BG[Math.min(4, real + planned)],
                          // selected wins over planned (one outline per cell)
                          sel?.date === d
                            ? "outline-2 -outline-offset-1 outline-kumo-contrast"
                            : planned
                              ? PLANNED
                              : "",
                        ].join(" ")}
                        onClick={(e) => {
                          setSel({ date: d, el: e.currentTarget });
                          setN(cycleDay(d));
                        }}
                      />
                    }
                  />
                );
              })}
            </div>
          </div>
        </div>
      </div>

      <div className="flex flex-wrap items-center justify-between gap-3 text-xs text-kumo-subtle">
        <span>Click a day to step its planned commits 0 → 4.</span>
        <div className="flex items-center gap-4" aria-hidden="true">
          <span className="flex items-center gap-1">
            Less
            {LEVEL_BG.map((bg) => (
              <span key={bg} className={`size-3 rounded-[3px] ${bg}`} />
            ))}
            More
          </span>
          <span className="flex items-center gap-1.5">
            <span className={`size-3 rounded-[3px] bg-kumo-recessed ${PLANNED}`} />
            Planned
          </span>
        </div>
      </div>

      <Popover open={sel !== null} onOpenChange={(open) => !open && setSel(null)}>
        {sel && (
          <Popover.Content anchor={sel.el} side="bottom" className="flex w-60 flex-col gap-3">
            <div className="flex items-start justify-between gap-2">
              <Popover.Title className="m-0 text-sm font-semibold">{prettyDay(sel.date, true)}</Popover.Title>
              <Popover.Close
                render={<Button variant="ghost" size="xs" shape="square" icon={XIcon} aria-label="close" />}
              />
            </div>
            <Text variant="secondary" size="sm">
              {plural(counts[sel.date] ?? 0, "commit")} on GitHub
            </Text>
            <Input
              type="number"
              size="sm"
              label="Planned commits"
              description="0 removes the day from the plan (max 4)"
              min={0}
              max={4}
              aria-label={`planned commits for ${sel.date}`}
              value={n}
              onChange={(e) => {
                // typing "999" clamps to the 0–4 level range, matching
                // the server-validated cells
                const v = clampLevel(+e.target.value);
                setN(v);
                setDay(sel.date, v);
              }}
            />
          </Popover.Content>
        )}
      </Popover>
    </TooltipProvider>
  );
}
