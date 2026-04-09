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
    reasoning_mode: "rules-mvp",
    reasoning_profile: "nrese-rules-mvp",
    reasoning_semantic_tier: "bounded-owl-rules",
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
    reasoning_profile: "nrese-rules-mvp",
    reasoning_semantic_tier: "bounded-owl-rules",
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
    mode: "rules-mvp",
    profile: "rules-mvp",
    capabilities: [
      {
        feature: "owl-property-chain-axioms",
        maturity: "bounded",
        enabled_by_default: false,
      },
    ],
    configured_policy: {
      preset: "bounded-owl",
      semantic_tier: "bounded-owl-rules",
      available_presets: ["rdfs-core", "bounded-owl"],
      feature_modes: [
        {
          feature: "owl-property-chain-axioms",
          mode: "enabled",
        },
      ],
      unsupported_constructs: "diagnose",
    },
    last_run: null,
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
});
