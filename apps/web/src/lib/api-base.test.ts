import { describe, expect, test } from "bun:test";

import { DEFAULT_API_BASE, resolveApiBase } from "./api-base";

describe("resolveApiBase", () => {
  test("widget ?api= param wins over session and env", () => {
    expect(
      resolveApiBase({
        param: "https://w.example",
        session: "https://s.example",
        env: "https://e.example",
      })
    ).toBe("https://w.example");
  });

  test("session beats env", () => {
    expect(resolveApiBase({ session: "https://s.example", env: "https://e.example" })).toBe(
      "https://s.example"
    );
  });

  test("env beats the default", () => {
    expect(resolveApiBase({ env: "https://e.example" })).toBe("https://e.example");
  });

  test("falls back to localhost:8080", () => {
    expect(resolveApiBase({})).toBe(DEFAULT_API_BASE);
  });

  test("invalid values fall through to the next source", () => {
    expect(resolveApiBase({ param: "not a url", session: "", env: "ftp://x" })).toBe(
      DEFAULT_API_BASE
    );
    expect(resolveApiBase({ param: "", session: "https://s.example" })).toBe("https://s.example");
  });

  test("javascript: URLs are rejected", () => {
    expect(resolveApiBase({ param: "javascript:alert(1)", env: "https://e.example" })).toBe(
      "https://e.example"
    );
  });

  test("trailing slashes are stripped", () => {
    expect(resolveApiBase({ env: "https://e.example/" })).toBe("https://e.example");
  });
});
