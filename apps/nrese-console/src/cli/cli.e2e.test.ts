// @vitest-environment node
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, test } from "vitest";

// The real CLI (`npm run cli`, src/cli/main.ts) as a process against a Graph Store
// Protocol stand-in that reads targets as nrese-server does (`http/rdf_payload.rs`:
// `?default`, or one `graph=`, where `urn:x-arq:DefaultGraph` and `urn:x-rdflib:default`
// also name the default graph, `nrese-sparql/src/compat.rs`). SYN-72 C-1107: a graph
// command acts on exactly the graph named, or refuses with no request sent.

const consoleRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const tsxPackage = join(consoleRoot, "node_modules", "tsx", "package.json");
const tsxBin = (() => {
  const bin = JSON.parse(readFileSync(tsxPackage, "utf8")).bin as string | Record<string, string>;
  return join(dirname(tsxPackage), typeof bin === "string" ? bin : bin.tsx);
})();

const SERVER_DEFAULT_ALIASES = ["urn:x-arq:DefaultGraph", "urn:x-rdflib:default"];
const DEFAULT_DATA = "<http://example.org/s> <http://example.org/p> <http://example.org/kept> .";
const NAMED = "https://example.org/g";

type Store = { graphs: Map<string, string>; requests: [string, string][] };

/** The stand-in's graph key: "" for the default graph. */
function target(url: URL): string | undefined {
  if (url.searchParams.has("default")) {
    return "";
  }
  const graph = url.searchParams.get("graph");
  if (graph === null) {
    return "";
  }
  return SERVER_DEFAULT_ALIASES.includes(graph) ? "" : graph;
}

let server: Server | undefined;

afterEach(async () => {
  await new Promise<void>((done) => (server ? server.close(() => done()) : done()));
  server = undefined;
});

async function start(): Promise<{ store: Store; base: string }> {
  const store: Store = {
    graphs: new Map([
      ["", DEFAULT_DATA],
      [NAMED, "<http://example.org/s> <http://example.org/p> <http://example.org/named> ."],
    ]),
    requests: [],
  };
  server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", () => {
      const url = new URL(request.url ?? "/", "http://stand-in");
      store.requests.push([request.method ?? "", `${url.pathname}${url.search}`]);
      const key = url.pathname === "/dataset/data" ? target(url) : undefined;
      if (key === undefined) {
        response.writeHead(404).end();
      } else if (request.method === "DELETE") {
        const existed = store.graphs.delete(key);
        response.writeHead(existed || key === "" ? 204 : 404).end();
      } else if (request.method === "PUT") {
        store.graphs.set(key, body);
        response.writeHead(204).end();
      } else {
        response.writeHead(405).end();
      }
    });
  });
  await new Promise<void>((done) => server!.listen(0, "127.0.0.1", () => done()));
  const { port } = server.address() as AddressInfo;
  return { store, base: `http://127.0.0.1:${port}` };
}

async function cli(base: string, args: string[]): Promise<{ code: number | null; stderr: string }> {
  const env = { ...process.env };
  delete env.NRESE_API_TOKEN;
  delete env.NRESE_API_BASE_URL;
  const child = spawn(process.execPath, [tsxBin, "src/cli/main.ts", ...args, "--base-url", base], {
    cwd: consoleRoot,
    env,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stderr = "";
  child.stderr.on("data", (chunk) => (stderr += chunk));
  child.stdout.resume();
  const code = await new Promise<number | null>((done) => child.on("close", done));
  return { code, stderr };
}

const PAYLOAD = "<http://example.org/s> <http://example.org/p> <http://example.org/new> .";

describe("the CLI's graph target, end to end", () => {
  test("graph-delete --graph-iri deletes that graph, one DELETE, DEFAULT kept", async () => {
    const { store, base } = await start();
    const run = await cli(base, ["graph-delete", "--graph-iri", NAMED]);
    expect(store.requests).toEqual([["DELETE", `/dataset/data?graph=${encodeURIComponent(NAMED)}`]]);
    expect(run.code, run.stderr).toBe(0);
    expect(store.graphs.get("")).toBe(DEFAULT_DATA);
    expect(store.graphs.has(NAMED)).toBe(false);
  }, 30_000);

  test("graph-write PUT --graph-iri replaces that graph, one PUT, DEFAULT kept", async () => {
    const { store, base } = await start();
    const run = await cli(base, ["graph-write", "--method", "PUT", "--text", PAYLOAD, "--graph-iri", NAMED]);
    expect(store.requests).toEqual([["PUT", `/dataset/data?graph=${encodeURIComponent(NAMED)}`]]);
    expect(run.code, run.stderr).toBe(0);
    expect(store.graphs.get("")).toBe(DEFAULT_DATA);
    expect(store.graphs.get(NAMED)).toBe(PAYLOAD);
  }, 30_000);

  // R1-F1: a named target the server reads as the default graph.
  test.each(
    SERVER_DEFAULT_ALIASES.flatMap((alias) => [
      ["graph-delete", "--graph", "named", "--graph-iri", alias],
      ["graph-write", "--method", "PUT", "--text", PAYLOAD, "--graph", "named", "--graph-iri", alias],
    ]),
  )("%s … refuses a named target that names DEFAULT: no request, DEFAULT kept", async (...args) => {
    const { store, base } = await start();
    const run = await cli(base, args);
    expect(store.requests).toEqual([]);
    expect(run.code).toBe(1);
    expect(run.stderr).toMatch(/default graph/i);
    expect(store.graphs.get("")).toBe(DEFAULT_DATA);
  }, 30_000);

  test.each([
    [["graph-delete"]],
    [["graph-delete", "--graph", "default", "--graph-iri", NAMED]],
  ])("%j refuses: no request, DEFAULT kept", async (args) => {
    const { store, base } = await start();
    const run = await cli(base, args);
    expect(store.requests).toEqual([]);
    expect(run.code).toBe(1);
    expect(store.graphs.get("")).toBe(DEFAULT_DATA);
  }, 30_000);
});
