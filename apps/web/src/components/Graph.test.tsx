import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";

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
});
