export type RuntimeSnapshot = {
  status: string;
  ready: boolean;
  revision: number;
  quad_count: number;
  named_graph_count: number;
  store_mode: string;
  durability: string;
  reasoning_mode: string;
  reasoning_profile: string;
  reasoning_preset?: string | null;
  ontology_path?: string | null;
  version: string;
};

export type Capabilities = {
  user_console_path: string;
  operator_ui_path: string;
  query_endpoint: string;
  update_endpoint: string;
  tell_endpoint: string;
  graph_store_endpoint: string;
  ai_status_endpoint: string;
  ai_query_suggestions_endpoint: string;
  available_reasoning_presets: string[];
  reasoning_preset?: string | null;
  tell_enabled: boolean;
  ai_query_suggestions_enabled: boolean;
  ai_provider: string;
};

export type AiStatus = {
  enabled: boolean;
  provider: string;
  model?: string | null;
};

export type QuerySuggestion = {
  title: string;
  explanation: string;
  sparql: string;
};

export type QuerySuggestionResponse = {
  provider: string;
  model: string;
  suggestions: QuerySuggestion[];
};

export type OutputState = {
  title: string;
  status: "idle" | "success" | "error";
  body: string;
};
