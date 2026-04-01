import { NRESE_ENDPOINTS, buildGraphQuery, type GraphMode } from "./endpoints";
import { fetchJson, fetchText, type FetchLike } from "./http";
import type {
  AiStatus,
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
    return fetchJson<RuntimeSnapshot>(
      this.fetchImpl,
      this.baseUrl,
      NRESE_ENDPOINTS.runtimeSnapshot,
    );
  }

  async getCapabilities(): Promise<Capabilities> {
    return fetchJson<Capabilities>(
      this.fetchImpl,
      this.baseUrl,
      NRESE_ENDPOINTS.capabilities,
    );
  }

  async getReasoningDiagnostics(
    endpoint = NRESE_ENDPOINTS.reasoningDiagnostics,
  ): Promise<ReasoningDiagnostics> {
    return fetchJson<ReasoningDiagnostics>(this.fetchImpl, this.baseUrl, endpoint);
  }

  async getAiStatus(): Promise<AiStatus> {
    return fetchJson<AiStatus>(
      this.fetchImpl,
      this.baseUrl,
      NRESE_ENDPOINTS.aiStatus,
    );
  }

  async getQuerySuggestions(payload: {
    prompt: string;
    locale: string;
    current_query?: string;
  }): Promise<QuerySuggestionResponse> {
    return fetchJson<QuerySuggestionResponse>(
      this.fetchImpl,
      this.baseUrl,
      NRESE_ENDPOINTS.aiQuerySuggestions,
      {
        method: "POST",
        headers: this.mergeHeaders({
          "Content-Type": "application/json",
        }),
        body: JSON.stringify(payload),
      },
    );
  }

  async runQuery(query: string, accept: string) {
    return fetchText(this.fetchImpl, this.baseUrl, NRESE_ENDPOINTS.query, {
      method: "POST",
      headers: this.mergeHeaders({
        "Content-Type": "application/sparql-query",
        Accept: accept,
      }),
      body: query,
    });
  }

  async runUpdate(update: string) {
    return fetchText(this.fetchImpl, this.baseUrl, NRESE_ENDPOINTS.update, {
      method: "POST",
      headers: this.mergeHeaders({
        "Content-Type": "application/sparql-update",
      }),
      body: update,
    });
  }

  async runTell(body: string, graphMode: GraphMode, graphIri: string) {
    return fetchText(
      this.fetchImpl,
      this.baseUrl,
      `${NRESE_ENDPOINTS.tell}${buildGraphQuery(graphMode, graphIri)}`,
      {
        method: "POST",
        headers: this.mergeHeaders({
          "Content-Type": "text/turtle",
        }),
        body,
      },
    );
  }

  async readGraph(graphMode: GraphMode, graphIri: string) {
    return fetchText(
      this.fetchImpl,
      this.baseUrl,
      `${NRESE_ENDPOINTS.graphStore}${buildGraphQuery(graphMode, graphIri)}`,
      {
        headers: this.mergeHeaders({
          Accept: "text/turtle",
        }),
      },
    );
  }

  async writeGraph(
    method: "PUT" | "POST",
    body: string,
    graphMode: GraphMode,
    graphIri: string,
  ) {
    return fetchText(
      this.fetchImpl,
      this.baseUrl,
      `${NRESE_ENDPOINTS.graphStore}${buildGraphQuery(graphMode, graphIri)}`,
      {
        method,
        headers: this.mergeHeaders({
          "Content-Type": "text/turtle",
        }),
        body,
      },
    );
  }

  async deleteGraph(graphMode: GraphMode, graphIri: string) {
    return fetchText(
      this.fetchImpl,
      this.baseUrl,
      `${NRESE_ENDPOINTS.graphStore}${buildGraphQuery(graphMode, graphIri)}`,
      {
        method: "DELETE",
      },
    );
  }

  private mergeHeaders(headers?: Record<string, string>): Record<string, string> {
    return {
      ...this.defaultHeaders,
      ...headers,
    };
  }
}
