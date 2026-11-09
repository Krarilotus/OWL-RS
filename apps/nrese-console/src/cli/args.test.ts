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

// SYN-72 C-1107: a graph command acts on exactly the graph the user named, or refuses
// before any request (design note 2026-11-08-p2-b8-design.md, invariant 1–5).
describe("graph targets", () => {
  const iri = "https://example.org/g";
  const named = { kind: "named", iri };

  test.each([
    ["graph-delete", []],
    ["graph-write", ["--method", "PUT", "--text", "<a> <b> <c> ."]],
    ["graph-write", ["--method", "POST", "--text", "<a> <b> <c> ."]],
    ["tell", ["--text", "<a> <b> <c> ."]],
    ["graph-read", []],
  ])("%s %j --graph-iri names that graph", (command, extra) => {
    const parsed = parseCliArgs([command, ...extra, "--graph-iri", iri]);
    expect(parsed.command).toMatchObject({ target: named });
  });

  test("--graph named --graph-iri names that graph", () => {
    const parsed = parseCliArgs(["graph-delete", "--graph", "named", "--graph-iri", iri]);
    expect(parsed.command).toMatchObject({ target: named });
  });

  test("--graph default names the default graph", () => {
    const parsed = parseCliArgs(["graph-delete", "--graph", "default"]);
    expect(parsed.command).toMatchObject({ target: { kind: "default" } });
  });

  test.each([
    [["--graph", "default", "--graph-iri", iri], /default.*graph-iri|graph-iri.*default/i],
    [["--graph-iri", iri, "--graph", "default"], /default.*graph-iri|graph-iri.*default/i],
    [["--graph-iri", iri, "--graph-iri", "https://example.org/h"], /twice/i],
    [["--graph", "named", "--graph", "default", "--graph-iri", iri], /twice/i],
  ])("refuses the ambiguous target %j", (flags, message) => {
    expect(() => parseCliArgs(["graph-delete", ...flags])).toThrow(message);
  });

  test.each([
    ["graph-delete", []],
    ["graph-write", ["--text", "<a> <b> <c> ."]],
    ["graph-write", ["--method", "PUT", "--text", "<a> <b> <c> ."]],
  ])("%s %j without a target refuses: it drops or replaces a graph", (command, extra) => {
    expect(() => parseCliArgs([command, ...extra])).toThrow(/--graph default|--graph-iri/);
  });

  test.each([
    ["tell", ["--text", "<a> <b> <c> ."]],
    ["graph-read", []],
    ["graph-write", ["--method", "POST", "--text", "<a> <b> <c> ."]],
  ])("%s %j without a target adds to or reads the default graph", (command, extra) => {
    const parsed = parseCliArgs([command, ...extra]);
    expect(parsed.command).toMatchObject({ target: { kind: "default" } });
  });

  test.each([
    ["update", ["--text", "CLEAR DEFAULT", "--graph-iri", iri]],
    ["query", ["--text", "ASK {}", "--graph", "default"]],
  ])("%s %j refuses graph flags it can't honour", (command, args) => {
    expect(() => parseCliArgs([command, ...args])).toThrow(/takes no graph/i);
  });
});
