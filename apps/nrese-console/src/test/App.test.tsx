import { render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import App from "../App";

vi.mock("../lib/api", () => ({
  getRuntimeSnapshot: async () => ({
    status: "ready",
    ready: true,
    revision: 2,
    quad_count: 42,
    named_graph_count: 1,
    store_mode: "in-memory",
    durability: "ephemeral",
    reasoning_mode: "rules-mvp",
    reasoning_profile: "rules-mvp",
    ontology_path: null,
    version: "0.1.0",
  }),
  getCapabilities: async () => ({
    user_console_path: "/console",
    operator_ui_path: "/ops",
    query_endpoint: "/dataset/query",
    update_endpoint: "/dataset/update",
    tell_endpoint: "/dataset/tell",
    graph_store_endpoint: "/dataset/data",
    ai_status_endpoint: "/api/ai/status",
    ai_query_suggestions_endpoint: "/api/ai/query-suggestions",
    tell_enabled: true,
    ai_query_suggestions_enabled: true,
    ai_provider: "gemini",
  }),
  getAiStatus: async () => ({
    enabled: true,
    provider: "gemini",
    model: "gemini-2.5-flash",
  }),
  getQuerySuggestions: async () => ({
    provider: "gemini",
    model: "gemini-2.5-flash",
    suggestions: [],
  }),
  runQuery: async () => ({ ok: true, status: 200, body: "ok" }),
  runUpdate: async () => ({ ok: true, status: 204, body: "" }),
  runTell: async () => ({ ok: true, status: 204, body: "" }),
  readGraph: async () => ({ ok: true, status: 200, body: "" }),
  writeGraph: async () => ({ ok: true, status: 204, body: "" }),
  deleteGraph: async () => ({ ok: true, status: 204, body: "" }),
}));

test("renders console sections", async () => {
  render(
    <QueryClientProvider client={new QueryClient()}>
      <App />
    </QueryClientProvider>,
  );

  expect(await screen.findByRole("heading", { name: /NRESE Console/i })).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: /Runtime snapshot/i })).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: /AI query assistant/i })).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: /Guided examples/i })).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: /Knowledge workbench/i })).toBeInTheDocument();
});
