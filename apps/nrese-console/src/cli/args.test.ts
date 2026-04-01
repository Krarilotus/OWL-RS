import { describe, expect, test } from "vitest";

import { parseCliArgs } from "./args";

describe("parseCliArgs", () => {
  test("parses a query command with explicit base URL", () => {
    const parsed = parseCliArgs([
      "query",
      "--base-url",
      "http://127.0.0.1:8080",
      "--text",
      "SELECT * WHERE { ?s ?p ?o } LIMIT 1",
    ]);

    expect(parsed.baseUrl).toBe("http://127.0.0.1:8080");
    expect(parsed.command.kind).toBe("query");
  });

  test("requires graph iri for named graph operations", () => {
    expect(() =>
      parseCliArgs(["graph-read", "--graph", "named"]),
    ).toThrow(/graph-iri/i);
  });
});
