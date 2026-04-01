export const NRESE_ENDPOINTS = {
  runtimeSnapshot: "/ops/api/health/extended",
  capabilities: "/ops/api/capabilities",
  reasoningDiagnostics: "/ops/api/diagnostics/reasoning",
  aiStatus: "/api/ai/status",
  aiQuerySuggestions: "/api/ai/query-suggestions",
  query: "/dataset/query",
  update: "/dataset/update",
  tell: "/dataset/tell",
  graphStore: "/dataset/data",
} as const;

export type GraphMode = "default" | "named";

export function buildGraphQuery(graphMode: GraphMode, graphIri: string): string {
  return graphMode === "default"
    ? "?default"
    : `?graph=${encodeURIComponent(graphIri)}`;
}
