import { afterEach, describe, expect, test, vi } from "vitest";

// The console's API adapter (lib/api.ts, what App.tsx calls), with the browser's fetch.
// SYN-72 R1-F1: a named target the server reads as the default graph is refused before
// any request; an ordinary named graph gets exactly one request to it.

const ALIASES = ["urn:x-arq:DefaultGraph", "urn:x-rdflib:default"];

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

async function adapter() {
  const fetchImpl = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
    new Response(null, { status: 204 }),
  );
  vi.stubGlobal("fetch", fetchImpl);
  vi.resetModules();
  return { api: await import("./api"), fetchImpl };
}

describe("console graph actions", () => {
  test.each(ALIASES)("refuse named %s for delete and replace, sending nothing", async (alias) => {
    const { api, fetchImpl } = await adapter();
    await expect(api.deleteGraph("named", alias)).rejects.toThrow(/default graph/i);
    await expect(api.writeGraph("PUT", "", "named", alias)).rejects.toThrow(/default graph/i);
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  test("send one request to an ordinary named graph", async () => {
    const { api, fetchImpl } = await adapter();
    await api.deleteGraph("named", "https://example.org/g");
    expect(fetchImpl.mock.calls.map(([input, init]) => [init?.method, input])).toEqual([
      ["DELETE", "/dataset/data?graph=https%3A%2F%2Fexample.org%2Fg"],
    ]);
  });
});
