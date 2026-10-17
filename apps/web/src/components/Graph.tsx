"use client";

import { Badge } from "@cloudflare/kumo/components/badge";
import { Banner } from "@cloudflare/kumo/components/banner";
import { RefreshButton } from "@cloudflare/kumo/components/button";
import { LayerCard } from "@cloudflare/kumo/components/layer-card";
import { Toasty } from "@cloudflare/kumo/components/toast";
import { WarningCircleIcon } from "@phosphor-icons/react/dist/ssr";

import ConnectionPanel from "./ConnectionPanel";
import ContributionGrid from "./ContributionGrid";
import PlanToolbar from "./PlanToolbar";
import SchedulesPanel from "./SchedulesPanel";
import { type GraphState, useGraphState } from "./useGraphState";

type GraphProps = {
  tools?: boolean;
  /**
   * `?api=` URL param; seeds the sessionStorage override on mount. No route
   * forwards this: the widget must ignore `?api=` because it is embeddable
   * cross-origin, and a framed override would aim the visitor's PAT at any
   * host the framer picks.
   */
  apiParam?: string;
  /**
   * Pins the API base (test seam); defaults to the resolved
   * session → build-time env → default chain.
   */
  apiBase?: string;
};

// Light/dark comes from `data-mode` on an ancestor (see layout.tsx and the
// widget page), not from a prop: Kumo's tokens switch on it.
export default function Graph(props: GraphProps) {
  return (
    <Toasty>
      <GraphBody {...props} />
    </Toasty>
  );
}

function status(s: GraphState) {
  if (!s.pat) return { variant: "neutral", text: "Not connected" } as const;
  if (s.loading) return { variant: "info", text: "Loading…" } as const;
  if (s.graphErr) return { variant: "error", text: "Error" } as const;
  return { variant: "success", text: "Connected" } as const;
}

function Stat({ label, value }: { label: string; value: string | number }) {
  return (
    <div className="flex flex-col">
      <dt className="text-xs text-kumo-subtle">{label}</dt>
      <dd className="m-0 text-xl font-semibold tabular-nums text-kumo-default">{value}</dd>
    </div>
  );
}

function GraphBody({ tools = false, apiParam, apiBase }: GraphProps) {
  const s = useGraphState({ apiParam, apiBase });
  const st = status(s);
  const realTotal = Object.values(s.counts).reduce((sum, c) => sum + c, 0);
  const connected = st.variant === "success";

  return (
    // isolate: Kumo portals popups to <body>; a fresh stacking context here
    // keeps them above anything this tree layers.
    <div className="isolate flex flex-col gap-4">
      <LayerCard>
        <LayerCard.Secondary className="flex items-center justify-between gap-2">
          <div className="flex items-center gap-3">
            <span className="font-medium">Contribution graph</span>
            <Badge variant={st.variant} appearance="dot">
              {st.text}
            </Badge>
          </div>
          <RefreshButton
            variant="ghost"
            size="sm"
            aria-label="Reload graph"
            loading={s.loading}
            disabled={!s.pat}
            onClick={s.retry}
          />
        </LayerCard.Secondary>
        <LayerCard.Primary className="flex flex-col gap-4">
          <dl className="m-0 flex flex-wrap gap-x-10 gap-y-3">
            <Stat label="Commits, last 12 months" value={connected ? realTotal.toLocaleString("en-US") : "—"} />
            <Stat label="Planned commits" value={s.planCount} />
            <Stat label="Planned days" value={Object.keys(s.plan).length} />
          </dl>

          {s.graphErr && (
            <Banner
              variant="error"
              size="sm"
              icon={<WarningCircleIcon weight="fill" />}
              description={<span role="alert">{s.graphErr}</span>}
              action={<Banner.Action onClick={s.retry}>Retry</Banner.Action>}
            />
          )}
          {s.loading && (
            <span role="status" className="sr-only">
              loading graph…
            </span>
          )}

          <ContributionGrid
            counts={s.counts}
            plan={s.plan}
            setDay={s.setDay}
            cycleDay={s.cycleDay}
            loading={s.loading}
          />
        </LayerCard.Primary>
      </LayerCard>

      <div className="grid items-stretch gap-4 md:grid-cols-2">
        <ConnectionPanel
          pat={s.pat}
          setPat={s.setPat}
          apiInput={s.apiInput}
          setApiInput={s.setApiInput}
          apiBase={s.apiBase}
        />
        <PlanToolbar
          tools={tools}
          pat={s.pat}
          plan={s.plan}
          planCount={s.planCount}
          clearPlan={s.clearPlan}
          busy={s.busy}
          error={s.error}
          apply={s.apply}
          resetRepo={s.resetRepo}
          randomFill={s.randomFill}
          jitter={s.jitter}
          setJitter={s.setJitter}
        />
      </div>

      {tools && <SchedulesPanel pat={s.pat} apiBase={s.apiBase} />}
    </div>
  );
}
