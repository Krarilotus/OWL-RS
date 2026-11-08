import { describe, expect, test, vi } from "vitest";

import { NreseClient } from "./client";
import { DEFAULT_GRAPH, selectedGraphTarget } from "./endpoints";

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

  // SYN-72 C-1107: every graph request names exactly its target (Graph Store Protocol §4:
  // `?default` or one `graph=` parameter carrying the whole IRI).
  test.each([
    [{ kind: "named", iri: "https://example.org/g#x&y z" }, "?graph=https%3A%2F%2Fexample.org%2Fg%23x%26y%20z"],
    [{ kind: "default" }, "?default"],
  ] as const)("sends graph requests to %j", async (target, query) => {
    const fetchImpl = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
      new Response(null, { status: 204 }),
    );
    const client = new NreseClient({ fetchImpl: fetchImpl as typeof fetch });

    await client.deleteGraph(target);
    await client.writeGraph("PUT", "", target);
    await client.readGraph(target);
    await client.runTell("", target);

    expect(fetchImpl.mock.calls.map(([input]) => input)).toEqual([
      `/dataset/data${query}`,
      `/dataset/data${query}`,
      `/dataset/data${query}`,
      `/dataset/tell${query}`,
    ]);
  });

  // SYN-72 R1-F1: the server reads these named graphs as the default graph
  // (nrese-sparql/src/compat.rs); a named target naming one is refused before any request.
  test.each(["urn:x-arq:DefaultGraph", "urn:x-rdflib:default"])(
    "refuses a named target %s, sending nothing",
    async (iri) => {
      const fetchImpl = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) =>
        new Response(null, { status: 204 }),
      );
      const client = new NreseClient({ fetchImpl: fetchImpl as typeof fetch });
      const target = { kind: "named", iri } as const;

      await expect(client.deleteGraph(target)).rejects.toThrow(/default graph/i);
      await expect(client.writeGraph("PUT", "", target)).rejects.toThrow(/default graph/i);
      await expect(client.writeGraph("POST", "", target)).rejects.toThrow(/default graph/i);
      await expect(client.readGraph(target)).rejects.toThrow(/default graph/i);
      await expect(client.runTell("", target)).rejects.toThrow(/default graph/i);
      expect(fetchImpl).not.toHaveBeenCalled();
    },
  );

  // Guard (unchanged behaviour): the console's selector names the default graph in default
  // mode whatever its IRI field holds, and the typed IRI in named mode.
  test("maps the console's graph selector to its target", () => {
    expect(selectedGraphTarget("default", "http://example.com/g")).toEqual({ kind: "default" });
    expect(selectedGraphTarget("named", "http://example.com/g")).toEqual({
      kind: "named",
      iri: "http://example.com/g",
    });
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
    await client.runTell("", DEFAULT_GRAPH);
    await client.readGraph(DEFAULT_GRAPH);
    await client.writeGraph("PUT", "", DEFAULT_GRAPH);
    await client.deleteGraph(DEFAULT_GRAPH);

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
