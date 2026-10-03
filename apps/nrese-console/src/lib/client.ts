import { NRESE_ENDPOINTS, buildGraphQuery, type GraphMode } from "./endpoints";
import { fetchJson, fetchText, type FetchLike } from "./http";
import type {
  AiStatus,
  AutocompleteResponse,
  Capabilities,
  QuerySuggestionResponse,
  ReasoningDiagnostics,
  RuntimeSnapshot,
} from "./types";

type ClientOptions = {
  baseUrl?: string;
  defaultHeaders?: Record<string, string>;
  fetchImpl?: FetchLike;
};

export class NreseClient {
  private readonly baseUrl: string;
  private readonly defaultHeaders: Record<string, string>;
  private readonly fetchImpl: FetchLike;

  constructor(options: ClientOptions = {}) {
    this.baseUrl = options.baseUrl ?? "";
    this.defaultHeaders = options.defaultHeaders ?? {};
    this.fetchImpl = options.fetchImpl ?? fetch;
  }

  async getRuntimeSnapshot(): Promise<RuntimeSnapshot> {
    return this.json<RuntimeSnapshot>(NRESE_ENDPOINTS.runtimeSnapshot);
  }

  async getCapabilities(): Promise<Capabilities> {
    return this.json<Capabilities>(NRESE_ENDPOINTS.capabilities);
  }

  /** The server advertises the diagnostics path at runtime, so any path is accepted. */
  async getReasoningDiagnostics(
    endpoint: string = NRESE_ENDPOINTS.reasoningDiagnostics,
  ): Promise<ReasoningDiagnostics> {
    return this.json<ReasoningDiagnostics>(endpoint);
  }

  async getAiStatus(): Promise<AiStatus> {
    return this.json<AiStatus>(NRESE_ENDPOINTS.aiStatus);
  }

  async getQuerySuggestions(payload: {
    prompt: string;
    locale: string;
    current_query?: string;
  }): Promise<QuerySuggestionResponse> {
    return this.json<QuerySuggestionResponse>(NRESE_ENDPOINTS.aiQuerySuggestions, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    });
  }

  /** Resources whose labels' or local names' words begin with the words of `text`. */
  async autocomplete(text: string, limit = 10): Promise<AutocompleteResponse> {
    const params = new URLSearchParams({ q: text, limit: String(limit) });
    return this.json<AutocompleteResponse>(
      `${NRESE_ENDPOINTS.autocomplete}?${params.toString()}`,
    );
  }

  async runQuery(query: string, accept: string) {
    return this.text(NRESE_ENDPOINTS.query, {
      method: "POST",
      headers: { "Content-Type": "application/sparql-query", Accept: accept },
      body: query,
    });
  }

  async runUpdate(update: string) {
    return this.text(NRESE_ENDPOINTS.update, {
      method: "POST",
      headers: { "Content-Type": "application/sparql-update" },
      body: update,
    });
  }

  async runTell(body: string, graphMode: GraphMode, graphIri: string) {
    return this.text(`${NRESE_ENDPOINTS.tell}${buildGraphQuery(graphMode, graphIri)}`, {
      method: "POST",
      headers: { "Content-Type": "text/turtle" },
      body,
    });
  }

  async readGraph(graphMode: GraphMode, graphIri: string) {
    return this.text(
      `${NRESE_ENDPOINTS.graphStore}${buildGraphQuery(graphMode, graphIri)}`,
      { headers: { Accept: "text/turtle" } },
    );
  }

  async writeGraph(
    method: "PUT" | "POST",
    body: string,
    graphMode: GraphMode,
    graphIri: string,
  ) {
    return this.text(
      `${NRESE_ENDPOINTS.graphStore}${buildGraphQuery(graphMode, graphIri)}`,
      { method, headers: { "Content-Type": "text/turtle" }, body },
    );
  }

  async deleteGraph(graphMode: GraphMode, graphIri: string) {
    return this.text(
      `${NRESE_ENDPOINTS.graphStore}${buildGraphQuery(graphMode, graphIri)}`,
      { method: "DELETE" },
    );
  }

  /** Every request goes through these two, so each carries the configured headers
   * (authorisation, custom ones) under its own. */
  private json<T>(path: string, init: RequestInit = {}): Promise<T> {
    return fetchJson<T>(this.fetchImpl, this.baseUrl, path, this.withHeaders(init));
  }

  private text(path: string, init: RequestInit = {}) {
    return fetchText(this.fetchImpl, this.baseUrl, path, this.withHeaders(init));
  }

  private withHeaders(init: RequestInit): RequestInit {
    return {
      ...init,
      headers: this.mergeHeaders(init.headers as Record<string, string> | undefined),
    };
  }

  private mergeHeaders(headers?: Record<string, string>): Record<string, string> {
    return {
      ...this.defaultHeaders,
      ...headers,
    };
  }
}
