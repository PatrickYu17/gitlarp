import { Toasty } from "@cloudflare/kumo/components/toast";
import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";

import SchedulesPanel, { upcomingDays } from "./SchedulesPanel";

// The panel toasts on create/delete, so it needs Graph's toast provider.
const render = (pat: string) =>
  renderToStaticMarkup(
    <Toasty>
      <SchedulesPanel pat={pat} apiBase="http://x" />
    </Toasty>
  );

/** Kumo's Field generates ids, so check label → input wiring instead. */
function labelledInput(markup: string, label: string): string | null {
  const m = markup.match(new RegExp(`<label[^>]*for="([^"]+)"[^>]*>(?:<[^>]+>)*${label}<`));
  if (!m) return null;
  const input = markup.match(new RegExp(`<input[^>]*id="${m[1]}"[^>]*>`));
  return input ? input[0] : null;
}

describe("<SchedulesPanel />", () => {
  test("renders the schedules section, heading, and form", () => {
    const markup = render("p");
    expect(markup).toContain('id="schedules"');
    expect(markup).toContain("Schedules (auto-pilot)");
    expect(labelledInput(markup, "Min per day")).toContain('type="number"');
    expect(labelledInput(markup, "Max per day")).toContain('type="number"');
    expect(labelledInput(markup, "Catch-up days")).toContain('type="number"');
    expect(labelledInput(markup, "From")).toContain('type="date"');
    expect(labelledInput(markup, "To")).toContain('type="date"');
    expect(markup).toContain("Weekends");
    expect(markup).toContain("Create schedule");
  });

  test("without a PAT shows the hint, not the disabled message", () => {
    const markup = render("");
    expect(markup).toContain("enter a PAT above to manage schedules");
    expect(markup).not.toContain("Scheduling is disabled");
  });
});

describe("upcomingDays()", () => {
  // 2026-10-17 is a Saturday, 2026-10-18 a Sunday, 2026-10-19 a Monday.
  test("skips weekends when weekends: false", () => {
    const days = upcomingDays([{ weekends: false }], "2026-10-17");
    expect(days.has("2026-10-18")).toBe(false);
    expect(days.has("2026-10-19")).toBe(true);
  });

  test("includes weekends when weekends: true (and by default)", () => {
    expect(upcomingDays([{ weekends: true }], "2026-10-17").has("2026-10-18")).toBe(true);
    expect(upcomingDays([{}], "2026-10-17").has("2026-10-18")).toBe(true);
  });

  test("honours the spec range exactly", () => {
    const days = upcomingDays([{ from: "2026-11-01", to: "2026-11-05" }], "2026-10-17");
    expect([...days].sort()).toEqual([
      "2026-11-01",
      "2026-11-02",
      "2026-11-03",
      "2026-11-04",
      "2026-11-05",
    ]);
  });

  test("no specs means no days", () => {
    expect(upcomingDays([], "2026-10-17").size).toBe(0);
  });

  test("honours the horizon", () => {
    expect(upcomingDays([{}], "2026-10-17", 5).size).toBe(5);
  });

  test("unions specs with different ranges", () => {
    const days = upcomingDays(
      [
        { from: "2026-11-01", to: "2026-11-02" },
        { from: "2026-11-04", to: "2026-11-05" },
      ],
      "2026-10-17"
    );
    expect([...days].sort()).toEqual(["2026-11-01", "2026-11-02", "2026-11-04", "2026-11-05"]);
  });
});
