import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";

import { DEFAULT_API_BASE } from "../lib/api-base";

import Graph from "./Graph";

describe("<Graph />", () => {
  const markup = renderToStaticMarkup(<Graph />);

  test("renders the PAT input", () => {
    expect(markup).toContain('id="gitlarp-pat"');
    expect(markup).toContain('for="gitlarp-pat"');
    expect(markup).toContain("GitHub PAT");
  });

  test("renders the API URL input", () => {
    expect(markup).toContain('id="gitlarp-api-url"');
    expect(markup).toContain('for="gitlarp-api-url"');
    expect(markup).toContain("API URL");
  });

  test("renders 365 graph cells with aria-labels", () => {
    const labels = markup.match(/aria-label="/g) ?? [];
    expect(labels.length).toBe(365);
    // each cell label mentions commits planned
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
});
