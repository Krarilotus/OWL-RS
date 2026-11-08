import { describe, expect, test, vi } from "vitest";

import { NreseClient } from "./client";

describe("NreseClient", () => {
  test("builds query requests against the configured base URL", async () => {
    const fetchImpl = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      new Response("{}", {
        status: 200,
        headers: {
          "Content-Type": "application/json",
        },
      }),
    );

    const client = new NreseClient({
      baseUrl: "https://api.example.com",
      defaultHeaders: {
        Authorization: "Bearer token",
      },
      fetchImpl: fetchImpl as typeof fetch,
    });

    await client.runQuery("SELECT * WHERE { ?s ?p ?o } LIMIT 1", "application/sparql-results+json");

    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(fetchImpl.mock.calls[0]?.[0]).toBe("https://api.example.com/dataset/query");
    expect(fetchImpl.mock.calls[0]?.[1]).toMatchObject({
      method: "POST",
      headers: {
        Authorization: "Bearer token",
        "Content-Type": "application/sparql-query",
        Accept: "application/sparql-results+json",
      },
    });
  });

  // SYN-72 C-1105: the server states every query answer's completeness, `complete`
  // included; an answer that arrives without it was not shown complete.
  test.each([
    [{}, { status: "not-certified", header: "absent" }],
    [{ "NRESE-Completeness": "complete" }, { status: "complete", header: "complete" }],
  ])("reads a query answer with headers %j as %j", async (headers, completeness) => {
    const fetchImpl = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      new Response("true", { status: 200, headers }),
    );
    const client = new NreseClient({ fetchImpl: fetchImpl as typeof fetch });

    const response = await client.runQuery("ASK {}", "application/sparql-results+json");

    expect(response.completeness).toEqual(completeness);
  });

  test("leaves a failed query's completeness unset", async () => {
    const fetchImpl = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      new Response("no", { status: 400 }),
    );
    const client = new NreseClient({ fetchImpl: fetchImpl as typeof fetch });

    const response = await client.runQuery("ASK {", "application/sparql-results+json");

    expect(response).toMatchObject({ ok: false, status: 400 });
    expect(response.completeness).toBeUndefined();
  });

  test("asks for autocompletion with the typed text encoded", async () => {
    const fetchImpl = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      new Response(JSON.stringify({ suggestions: [{ iri: "http://e/x", label: "X", score: 1 }] }), {
        status: 200,
        headers: {
          "Content-Type": "application/json",
        },
      }),
    );
    const client = new NreseClient({ fetchImpl: fetchImpl as typeof fetch });

    const response = await client.autocomplete("albert ein&", 5);

    expect(fetchImpl.mock.calls[0]?.[0]).toBe("/dataset/autocomplete?q=albert+ein%26&limit=5");
    expect(response.suggestions[0]?.label).toBe("X");
  });

  test("sends the configured headers with every request", async () => {
    // The review of 3 October 2026 (C3): diagnostics reads and graph DELETE went without.
    const fetchImpl = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      new Response("{}", { status: 200, headers: { "Content-Type": "application/json" } }),
    );
    const client = new NreseClient({
      defaultHeaders: { Authorization: "Bearer token", "X-Tenant": "t1" },
      fetchImpl: fetchImpl as typeof fetch,
    });

    await client.getRuntimeSnapshot();
    await client.getCapabilities();
    await client.getReasoningDiagnostics();
    await client.getAiStatus();
    await client.getQuerySuggestions({ prompt: "p", locale: "en" });
    await client.autocomplete("x");
    await client.runQuery("ASK {}", "application/sparql-results+json");
    await client.runUpdate("INSERT DATA {}");
    await client.runTell("", "default", "");
    await client.readGraph("default", "");
    await client.writeGraph("PUT", "", "default", "");
    await client.deleteGraph("default", "");

    expect(fetchImpl).toHaveBeenCalledTimes(12);
    for (const [input, init] of fetchImpl.mock.calls) {
      expect(init?.headers, String(input)).toMatchObject({
        Authorization: "Bearer token",
        "X-Tenant": "t1",
      });
    }
    // A method's own headers stay alongside.
    expect(fetchImpl.mock.calls[11]?.[1]).toMatchObject({ method: "DELETE" });
  });
});
