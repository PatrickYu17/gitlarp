import { describe, expect, test } from "bun:test";

import Widget from "./page";

// The page is a plain async server component: calling it with a resolved
// searchParams promise yields the <Graph /> element it would render, so
// its props can be asserted without a renderer.
describe("/widget page", () => {
  test("ignores ?api= (framed pages must not aim the visitor's PAT elsewhere)", async () => {
    const el = await Widget({
      searchParams: Promise.resolve({ theme: "dark", api: "https://attacker.example" }),
    });
    expect(el.props.apiParam).toBeUndefined();
  });

  test("theme whitelist: 'dark' passes through, anything else falls back to light", async () => {
    const dark = await Widget({ searchParams: Promise.resolve({ theme: "dark" }) });
    expect(dark.props.theme).toBe("dark");
    const other = await Widget({ searchParams: Promise.resolve({ theme: "neon" }) });
    expect(other.props.theme).toBe("light");
    const none = await Widget({ searchParams: Promise.resolve({}) });
    expect(none.props.theme).toBe("light");
  });
});
