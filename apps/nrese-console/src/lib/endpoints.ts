export const NRESE_ENDPOINTS = {
  runtimeSnapshot: "/api/v1/health",
  capabilities: "/api/v1/capabilities",
  reasoningDiagnostics: "/api/v1/repositories/nrese/reasoning",
  aiStatus: "/api/v1/ai/status",
  aiQuerySuggestions: "/api/v1/ai/query-suggestions",
  query: "/dataset/query",
  update: "/dataset/update",
  tell: "/dataset/tell",
  graphStore: "/dataset/data",
  autocomplete: "/dataset/autocomplete",
} as const;

export type GraphMode = "default" | "named";

/** The one graph a Graph Store Protocol request acts on. A default target carries no IRI,
 * so no request can name one graph and act on another (SYN-72). */
export type GraphTarget = { kind: "default" } | { kind: "named"; iri: string };

export const DEFAULT_GRAPH: GraphTarget = { kind: "default" };

/** The console's graph selector as a target: its mode decides, the IRI field counts only
 * for a named graph. */
export function selectedGraphTarget(graphMode: GraphMode, graphIri: string): GraphTarget {
  return graphMode === "named" ? { kind: "named", iri: graphIri } : DEFAULT_GRAPH;
}

/** Named graphs the server reads as the default graph, for other stores' clients (Jena's and
 * rdflib's names): `nrese_sparql::compat::names_default_graph`, the one list; a test keeps
 * this copy equal to it. */
export const DEFAULT_GRAPH_ALIASES: readonly string[] = [
  "urn:x-arq:DefaultGraph",
  "urn:x-rdflib:default",
];

/** The request's query string for `target`, which every graph request is built from. A
 * named target the server would read as the default graph is refused here, before any
 * request: a named target acts on that named graph or on nothing (SYN-72 R1-F1). The server
 * checks the IRI and the graph's existence; the client sends what was named, whole. */
export function graphQuery(target: GraphTarget): string {
  if (target.kind === "default") {
    return "?default";
  }
  if (DEFAULT_GRAPH_ALIASES.includes(target.iri)) {
    throw new Error(
      `${target.iri} names the default graph on this server, not a named graph: ` +
        "target the default graph (--graph default) to mean it",
    );
  }
  return `?graph=${encodeURIComponent(target.iri)}`;
}
