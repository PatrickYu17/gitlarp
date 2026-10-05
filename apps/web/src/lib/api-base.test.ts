import { describe, expect, test } from "bun:test";

import { DEFAULT_API_BASE, isInsecureBase, resolveApiBase } from "./api-base";

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

describe("isInsecureBase", () => {
  test("plain http to a remote host is insecure", () => {
    expect(isInsecureBase("http://example.com")).toBe(true);
    expect(isInsecureBase("http://example.com:8080")).toBe(true);
    expect(isInsecureBase("http://example.com/some/prefix/")).toBe(true);
  });

  test("https is never insecure", () => {
    expect(isInsecureBase("https://example.com")).toBe(false);
    expect(isInsecureBase("https://example.com:8443")).toBe(false);
  });

  test("http to localhost or 127.0.0.1 is loopback, not insecure", () => {
    expect(isInsecureBase("http://localhost:8080")).toBe(false);
    expect(isInsecureBase("http://127.0.0.1:8080")).toBe(false);
    expect(isInsecureBase(DEFAULT_API_BASE)).toBe(false);
  });

  test("lookalike hosts are not treated as localhost", () => {
    expect(isInsecureBase("http://localhost.example.com")).toBe(true);
    expect(isInsecureBase("http://127.0.0.1.example.com")).toBe(true);
  });

  test("garbage input defaults to safe (false)", () => {
    expect(isInsecureBase("not a url")).toBe(false);
    expect(isInsecureBase("")).toBe(false);
    expect(isInsecureBase("ftp://example.com")).toBe(false);
  });
});
