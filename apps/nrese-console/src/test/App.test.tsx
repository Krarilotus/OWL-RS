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
    deployment_posture: "open-workbench",
    store_mode: "in-memory",
    durability: "ephemeral",
    reasoning_mode: "owl2-rl",
    reasoning_profile: "nrese-v2",
    reasoning_read_model: "materialised",
    reasoning_semantic_tier: "owl2-rl",
    ontology_path: null,
    version: "0.1.0",
  }),
  getCapabilities: async () => ({
    user_console_path: "/console",
    operator_ui_path: "/ops",
    reasoning_diagnostics_endpoint: "/ops/api/diagnostics/reasoning",
    query_endpoint: "/dataset/query",
    update_endpoint: "/dataset/update",
    tell_endpoint: "/dataset/tell",
    graph_store_endpoint: "/dataset/data",
    deployment_posture: "open-workbench",
    ai_status_endpoint: "/api/ai/status",
    ai_query_suggestions_endpoint: "/api/ai/query-suggestions",
    reasoning_profile: "nrese-v2",
    reasoning_read_model: "materialised",
    reasoning_semantic_tier: "owl2-rl",
    tell_enabled: true,
    ai_query_suggestions_enabled: true,
    ai_provider: "gemini",
  }),
  getAiStatus: async () => ({
    enabled: true,
    provider: "gemini",
    model: "gemini-2.5-flash",
  }),
  getReasoningDiagnostics: async () => ({
    revision: 2,
    mode: "owl2-rl",
    profile: "nrese-v2",
    read_model: "materialised",
    capabilities: [
      {
        feature: "owl-property-chain-axioms",
        maturity: "mvp",
        enabled_by_default: true,
      },
    ],
    last_run: {
      revision: 2,
      status: "completed",
      ruleset: "owl2-rl",
      inferred_triples: 3,
      inferred_inserted: 3,
      inferred_deleted: 0,
      consistency_violations: 0,
      rounds: 2,
      elapsed_micros: 120,
    },
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
  expect(screen.getByLabelText(/Language/i)).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: /Runtime snapshot/i })).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: /AI query assistant/i })).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: /Guided examples/i })).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: /Knowledge workbench/i })).toBeInTheDocument();
  expect(await screen.findByText(/Reasoning capabilities/i)).toBeInTheDocument();
  expect((await screen.findAllByText(/owl-property-chain-axioms/i)).length).toBeGreaterThan(0);
  expect(await screen.findByText(/Last commit-path run/i)).toBeInTheDocument();
  expect(screen.getByText(/Inferred statements/i)).toBeInTheDocument();
});
