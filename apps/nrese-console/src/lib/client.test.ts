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
});
