import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";

import { DEFAULT_API_BASE } from "../lib/api-base";

import Graph from "./Graph";

/**
 * The input a visible label points at. Kumo's Field generates the ids, so
 * assert the label → control wiring rather than fixed id strings.
 */
function labelledInput(markup: string, label: string): string | null {
  const m = markup.match(new RegExp(`<label[^>]*for="([^"]+)"[^>]*>(?:<[^>]+>)*${label}<`));
  if (!m) return null;
  const input = markup.match(new RegExp(`<input[^>]*id="${m[1]}"[^>]*>`));
  return input ? input[0] : null;
}

describe("<Graph />", () => {
  const markup = renderToStaticMarkup(<Graph />);

  test("renders the PAT input, labelled and masked", () => {
    const input = labelledInput(markup, "GitHub PAT");
    expect(input).not.toBeNull();
    expect(input).toContain('type="password"');
  });

  test("renders the API URL input, labelled", () => {
    const input = labelledInput(markup, "API URL");
    expect(input).not.toBeNull();
    expect(input).toContain('type="text"');
  });

  test("renders 365 graph cells with aria-labels", () => {
    expect(markup.match(/aria-label="[^"]*commits planned"/g)?.length).toBe(365);
  });

  test("discloses where the PAT goes, using the resolved base", () => {
    expect(markup).toContain(`PAT will be sent to: ${DEFAULT_API_BASE}`);
  });

  test("no cleartext warning for the default localhost base", () => {
    expect(markup).not.toContain("not HTTPS");
  });

  test("warns when the resolved base is a remote http:// host", () => {
    const m = renderToStaticMarkup(<Graph apiBase="http://remote.example" />);
    expect(m).toContain("PAT will be sent to: http://remote.example");
    expect(m).toContain("Warning: this API base is not HTTPS");
    expect(m).toContain("cleartext");
  });

  test("https base shows the destination without the cleartext warning", () => {
    const m = renderToStaticMarkup(<Graph apiBase="https://api.example" />);
    expect(m).toContain("PAT will be sent to: https://api.example");
    expect(m).not.toContain("not HTTPS");
  });

  test("http localhost base is not flagged (loopback is not cleartext)", () => {
    const m = renderToStaticMarkup(<Graph apiBase="http://localhost:9999" />);
    expect(m).toContain("PAT will be sent to: http://localhost:9999");
    expect(m).not.toContain("not HTTPS");
  });

  test("tools render adds the schedules panel, reset, and jitter", () => {
    const m = renderToStaticMarkup(<Graph tools />);
    expect(m).toContain('id="schedules"');
    expect(m).toContain("Reset (undo all)");
    expect(m).toContain('id="gitlarp-jitter"');
  });

  test("default render has none of the schedules panel, reset, or jitter", () => {
    expect(markup).not.toContain('id="schedules"');
    expect(markup).not.toContain("Reset (undo all)");
    expect(markup).not.toContain('id="gitlarp-jitter"');
  });
});
