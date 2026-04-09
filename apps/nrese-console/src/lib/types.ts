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
  reasoning_preset?: string | null;
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
  available_reasoning_presets: string[];
  reasoning_preset?: string | null;
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

export type ReasoningFeatureMode = {
  feature: string;
  mode: string;
};

export type ReasoningCapability = {
  feature: string;
  maturity: string;
  enabled_by_default: boolean;
};

export type ConfiguredReasoningPolicy = {
  preset: string;
  semantic_tier: string;
  available_presets: string[];
  feature_modes: ReasoningFeatureMode[];
  unsupported_constructs: string;
};

export type ReasoningCache = {
  execution_cache_hit: boolean;
  schema_cache_hit: boolean;
  execution_cache_entries: number;
  schema_cache_entries: number;
  execution_cache_capacity: number;
  schema_cache_capacity: number;
  execution_cache_hits_total: number;
  execution_cache_misses_total: number;
  schema_cache_hits_total: number;
  schema_cache_misses_total: number;
};

export type LastReasoningRun = {
  revision: number;
  status: string;
  inferred_triples: number;
  consistency_violations: number;
  cache: ReasoningCache;
};

export type ReasoningDiagnostics = {
  revision: number;
  mode: string;
  profile: string;
  capabilities: ReasoningCapability[];
  configured_policy?: ConfiguredReasoningPolicy | null;
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

export type OutputState = {
  title: string;
  status: "idle" | "success" | "error";
  body: string;
};
