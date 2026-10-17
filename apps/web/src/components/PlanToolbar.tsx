"use client";

import { Banner } from "@cloudflare/kumo/components/banner";
import { Button } from "@cloudflare/kumo/components/button";
import type { DateRange } from "@cloudflare/kumo/components/date-picker";
import { Input } from "@cloudflare/kumo/components/input";
import { LayerCard } from "@cloudflare/kumo/components/layer-card";
import { Popover } from "@cloudflare/kumo/components/popover";
import { Text } from "@cloudflare/kumo/components/text";
import { useKumoToastManager } from "@cloudflare/kumo/components/toast";
import {
  CalendarBlankIcon,
  ArrowCounterClockwiseIcon,
  ListChecksIcon,
  ShuffleIcon,
  WarningCircleIcon,
} from "@phosphor-icons/react/dist/ssr";
import dynamic from "next/dynamic";
import { useState } from "react";

import { clampLevel, fmt, prettyDay } from "../lib/graph";
import Toggle from "./Toggle";
import type { GraphState } from "./useGraphState";

// The calendar loads on first open, keeping it out of first-load JS.
const RangePicker = dynamic(() => import("./RangePicker"), {
  ssr: false,
  loading: () => <div className="h-72 w-64" aria-busy="true" />,
});

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

export default function PlanToolbar({
  tools,
  pat,
  plan,
  planCount,
  clearPlan,
  busy,
  error,
  apply,
  resetRepo,
  randomFill,
  jitter,
  setJitter,
}: { tools: boolean } & Pick<
  GraphState,
  | "pat"
  | "plan"
  | "planCount"
  | "clearPlan"
  | "busy"
  | "error"
  | "apply"
  | "resetRepo"
  | "randomFill"
  | "jitter"
  | "setJitter"
>) {
  const toasts = useKumoToastManager();
  const [range, setRange] = useState<DateRange | undefined>();
  const [min, setMin] = useState(1);
  const [max, setMax] = useState(4);

  // The picker runs in UTC (timeZone below), so fmt()'s UTC getters give
  // the calendar day the user clicked, never the day before.
  const from = range?.from ? fmt(range.from) : "";
  const to = range?.to ? fmt(range.to) : "";

  const planDays = Object.keys(plan).sort();

  const onApply = async () => {
    const outcome = await apply();
    // failures stay on screen as the error banner; only successes toast
    if (outcome?.ok) toasts.add({ title: outcome.text, variant: "success" });
  };

  const onReset = async () => {
    if (!window.confirm("Undo every larp commit? The default branch resets to its pre-larp state (gitlarp-base).")) {
      return;
    }
    const outcome = await resetRepo();
    if (outcome?.ok) toasts.add({ title: outcome.text, variant: "success" });
  };

  return (
    <LayerCard>
      <LayerCard.Secondary className="flex items-center gap-2 font-medium">
        <ListChecksIcon size={16} className="text-kumo-subtle" />
        Plan
      </LayerCard.Secondary>
      <LayerCard.Primary className="flex h-full flex-col gap-4">
        {planCount > 0 ? (
          <div className="flex flex-col gap-0.5">
            <p className="m-0 text-2xl font-semibold tabular-nums text-kumo-default">
              {plural(planCount, "commit")}
              <span className="text-base font-normal text-kumo-subtle"> across {plural(planDays.length, "day")}</span>
            </p>
            <Text variant="secondary" size="sm">
              {planDays.length === 1
                ? prettyDay(planDays[0])
                : `${prettyDay(planDays[0])} → ${prettyDay(planDays[planDays.length - 1])}`}
            </Text>
          </div>
        ) : (
          <Text variant="secondary" size="sm">
            Nothing planned yet. Click days on the graph
            {tools ? ", or random-fill a date range below." : "."}
          </Text>
        )}

        {tools && (
          <div className="flex flex-col gap-3 rounded-lg bg-kumo-recessed p-3">
            <Text bold size="sm">
              Random fill
            </Text>
            <Popover>
              <Popover.Trigger
                render={
                  <Button variant="outline" icon={CalendarBlankIcon} className="justify-start">
                    {from ? `${prettyDay(from)} → ${to ? prettyDay(to) : "…"}` : "Pick a date range"}
                  </Button>
                }
              />
              <Popover.Content side="bottom" align="start">
                <RangePicker selected={range} onChange={setRange} />
              </Popover.Content>
            </Popover>
            <div className="grid grid-cols-2 gap-3">
              <Input
                label="Min per day"
                type="number"
                size="sm"
                min={0}
                max={4}
                value={min}
                onChange={(e) => setMin(clampLevel(+e.target.value))}
              />
              <Input
                label="Max per day"
                type="number"
                size="sm"
                min={0}
                max={4}
                value={max}
                onChange={(e) => setMax(clampLevel(+e.target.value))}
              />
            </div>
            <Button
              variant="secondary"
              icon={ShuffleIcon}
              disabled={busy || !from || !to}
              onClick={() => randomFill(from, to, min, max)}
            >
              Random fill (into plan)
            </Button>
          </div>
        )}

        {error && (
          <Banner
            variant="error"
            size="sm"
            icon={<WarningCircleIcon weight="fill" />}
            description={<span role="alert">{error}</span>}
          />
        )}

        <div className="mt-auto flex flex-col gap-2">
          {tools && (
            <Toggle
              label="Jitter times"
              title="Stamp each commit at a random time of day instead of noon UTC."
              checked={jitter}
              onChange={setJitter}
            />
          )}
          <div className="flex flex-wrap justify-end gap-2">
            {tools && (
              <Button
                variant="secondary-destructive"
                icon={ArrowCounterClockwiseIcon}
                disabled={busy || !pat}
                onClick={onReset}
                className="mr-auto"
              >
                Reset (undo all)
              </Button>
            )}
            <Button variant="ghost" disabled={busy || !planCount} onClick={clearPlan}>
              Clear plan
            </Button>
            <Button variant="primary" loading={busy} disabled={busy || !pat || !planCount} onClick={onApply}>
              Apply plan ({plural(planCount, "commit")})
            </Button>
          </div>
          {!pat && planCount > 0 && (
            <div className="text-right">
              <Text variant="secondary" size="xs">
                Add a GitHub PAT to apply the plan.
              </Text>
            </div>
          )}
        </div>
      </LayerCard.Primary>
    </LayerCard>
  );
}
