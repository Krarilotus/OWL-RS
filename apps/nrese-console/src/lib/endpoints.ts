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

/** The request's query string for `target`. The server checks the IRI and the graph's
 * existence; the client sends what was named, whole. */
export function graphQuery(target: GraphTarget): string {
  return target.kind === "default"
    ? "?default"
    : `?graph=${encodeURIComponent(target.iri)}`;
}
