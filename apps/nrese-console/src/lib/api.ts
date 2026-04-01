import { NreseClient } from "./client";
import { resolveBrowserApiBaseUrl } from "./runtimeConfig";
import type {
  AiStatus,
  Capabilities,
  QuerySuggestionResponse,
  ReasoningDiagnostics,
  RuntimeSnapshot,
} from "./types";

let browserClient: NreseClient | undefined;

export function getBrowserClient(): NreseClient {
  browserClient ??= new NreseClient({
    baseUrl: resolveBrowserApiBaseUrl(),
  });
  return browserClient;
}

export async function getRuntimeSnapshot(): Promise<RuntimeSnapshot> {
  return getBrowserClient().getRuntimeSnapshot();
}

export async function getCapabilities(): Promise<Capabilities> {
  return getBrowserClient().getCapabilities();
}

export async function getReasoningDiagnostics(
  endpoint = "/ops/api/diagnostics/reasoning",
): Promise<ReasoningDiagnostics> {
  return getBrowserClient().getReasoningDiagnostics(endpoint);
}

export async function getAiStatus(): Promise<AiStatus> {
  return getBrowserClient().getAiStatus();
}

export async function getQuerySuggestions(payload: {
  prompt: string;
  locale: string;
  current_query?: string;
}): Promise<QuerySuggestionResponse> {
  return getBrowserClient().getQuerySuggestions(payload);
}

export async function runQuery(query: string, accept: string) {
  return getBrowserClient().runQuery(query, accept);
}

export async function runUpdate(update: string) {
  return getBrowserClient().runUpdate(update);
}

export async function runTell(
  body: string,
  graphMode: "default" | "named",
  graphIri: string,
) {
  return getBrowserClient().runTell(body, graphMode, graphIri);
}

export async function readGraph(
  graphMode: "default" | "named",
  graphIri: string,
) {
  return getBrowserClient().readGraph(graphMode, graphIri);
}

export async function writeGraph(
  method: "PUT" | "POST",
  body: string,
  graphMode: "default" | "named",
  graphIri: string,
) {
  return getBrowserClient().writeGraph(method, body, graphMode, graphIri);
}

export async function deleteGraph(
  graphMode: "default" | "named",
  graphIri: string,
) {
  return getBrowserClient().deleteGraph(graphMode, graphIri);
}
