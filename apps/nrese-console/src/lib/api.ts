import { fetchJson, fetchText } from "./http";
import type {
  AiStatus,
  Capabilities,
  QuerySuggestionResponse,
  ReasoningDiagnostics,
  RuntimeSnapshot,
} from "./types";

export async function getRuntimeSnapshot(): Promise<RuntimeSnapshot> {
  return fetchJson<RuntimeSnapshot>("/ops/api/health/extended");
}

export async function getCapabilities(): Promise<Capabilities> {
  return fetchJson<Capabilities>("/ops/api/capabilities");
}

export async function getReasoningDiagnostics(
  endpoint = "/ops/api/diagnostics/reasoning",
): Promise<ReasoningDiagnostics> {
  return fetchJson<ReasoningDiagnostics>(endpoint);
}

export async function getAiStatus(): Promise<AiStatus> {
  return fetchJson<AiStatus>("/api/ai/status");
}

export async function getQuerySuggestions(payload: {
  prompt: string;
  locale: string;
  current_query?: string;
}): Promise<QuerySuggestionResponse> {
  return fetchJson<QuerySuggestionResponse>("/api/ai/query-suggestions", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify(payload),
  });
}

export async function runQuery(query: string, accept: string) {
  return fetchText("/dataset/query", {
    method: "POST",
    headers: {
      "Content-Type": "application/sparql-query",
      Accept: accept,
    },
    body: query,
  });
}

export async function runUpdate(update: string) {
  return fetchText("/dataset/update", {
    method: "POST",
    headers: {
      "Content-Type": "application/sparql-update",
    },
    body: update,
  });
}

export async function runTell(
  body: string,
  graphMode: "default" | "named",
  graphIri: string,
) {
  const suffix =
    graphMode === "default" ? "?default" : `?graph=${encodeURIComponent(graphIri)}`;
  return fetchText(`/dataset/tell${suffix}`, {
    method: "POST",
    headers: {
      "Content-Type": "text/turtle",
    },
    body,
  });
}

export async function readGraph(
  graphMode: "default" | "named",
  graphIri: string,
) {
  const suffix =
    graphMode === "default" ? "?default" : `?graph=${encodeURIComponent(graphIri)}`;
  return fetchText(`/dataset/data${suffix}`, {
    headers: {
      Accept: "text/turtle",
    },
  });
}

export async function writeGraph(
  method: "PUT" | "POST",
  body: string,
  graphMode: "default" | "named",
  graphIri: string,
) {
  const suffix =
    graphMode === "default" ? "?default" : `?graph=${encodeURIComponent(graphIri)}`;
  return fetchText(`/dataset/data${suffix}`, {
    method,
    headers: {
      "Content-Type": "text/turtle",
    },
    body,
  });
}

export async function deleteGraph(
  graphMode: "default" | "named",
  graphIri: string,
) {
  const suffix =
    graphMode === "default" ? "?default" : `?graph=${encodeURIComponent(graphIri)}`;
  return fetchText(`/dataset/data${suffix}`, {
    method: "DELETE",
  });
}
