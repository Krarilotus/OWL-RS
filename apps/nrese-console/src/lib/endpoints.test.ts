// @vitest-environment node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, test } from "vitest";

import { DEFAULT_GRAPH_ALIASES } from "./endpoints";

// The server's list is the one source (`nrese_sparql::compat::names_default_graph`); the
// console's copy must name the same IRIs, so a name added there is refused here too.
describe("default graph aliases", () => {
  test("are the server's names_default_graph list", () => {
    const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "..");
    const compat = readFileSync(join(repo, "crates", "nrese-sparql", "src", "compat.rs"), "utf8");
    const body = /pub fn names_default_graph\(iri: &str\) -> bool \{([^}]*)\}/.exec(compat)?.[1];
    expect(body, "names_default_graph in compat.rs").toBeDefined();
    const names = [...body!.matchAll(/iri == ([A-Z_]+)/g)].map(([, name]) => name);
    expect(names.length, "a constant compared in names_default_graph").toBeGreaterThan(0);
    // Every comparison is with a named constant; a literal would escape this check.
    expect(body!.replace(/iri == [A-Z_]+/g, "").replace(/\|\||\s/g, "")).toBe("");
    const server = names.map((name) => {
      const value = new RegExp(`pub const ${name}: &str = "([^"]*)";`).exec(compat)?.[1];
      expect(value, `${name} in compat.rs`).toBeDefined();
      return value!;
    });
    expect([...DEFAULT_GRAPH_ALIASES].sort()).toEqual(server.sort());
  });
});
