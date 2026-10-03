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

export function buildGraphQuery(graphMode: GraphMode, graphIri: string): string {
  return graphMode === "default"
    ? "?default"
    : `?graph=${encodeURIComponent(graphIri)}`;
}
