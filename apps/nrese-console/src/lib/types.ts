export type RuntimeSnapshot = {
  status: string;
  ready: boolean;
  revision: number;
  quad_count: number;
  named_graph_count: number;
  deployment_posture: string;
  store_mode: string;
  durability: string;
  reasoning_mode: string;
  reasoning_profile: string;
  reasoning_read_model: string;
  reasoning_semantic_tier: string;
  ontology_path?: string | null;
  version: string;
};

export type Capabilities = {
  user_console_path: string;
  operator_ui_path?: string | null;
  reasoning_diagnostics_endpoint?: string | null;
  query_endpoint: string;
  update_endpoint: string;
  tell_endpoint: string;
  graph_store_endpoint: string;
  admin_backup_endpoint?: string | null;
  admin_restore_endpoint?: string | null;
  metrics_endpoint?: string | null;
  deployment_posture: string;
  ai_status_endpoint: string;
  ai_query_suggestions_endpoint: string;
  reasoning_profile: string;
  reasoning_read_model: string;
  reasoning_semantic_tier: string;
  tell_enabled: boolean;
  graph_write_enabled?: boolean;
  admin_surface_enabled?: boolean;
  operator_surface_enabled?: boolean;
  metrics_enabled?: boolean;
  ai_query_suggestions_enabled: boolean;
  ai_provider: string;
};

export type AiStatus = {
  enabled: boolean;
  provider: string;
  model?: string | null;
};

export type ReasoningCapability = {
  feature: string;
  maturity: string;
  enabled_by_default: boolean;
};

export type LastReasoningRun = {
  revision: number;
  status: string;
  ruleset: string;
  inferred_triples: number;
  inferred_inserted: number;
  inferred_deleted: number;
  consistency_violations: number;
  rounds: number;
  elapsed_micros: number;
};

export type ReasoningDiagnostics = {
  revision: number;
  mode: string;
  profile: string;
  read_model: string;
  capabilities: ReasoningCapability[];
  last_run?: LastReasoningRun | null;
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

export type ResourceSuggestion = {
  iri: string;
  label?: string | null;
  score: number;
};

export type AutocompleteResponse = {
  suggestions: ResourceSuggestion[];
};

export type OutputState = {
  title: string;
  status: "idle" | "success" | "error";
  body: string;
};
