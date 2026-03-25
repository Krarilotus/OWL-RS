import type { AppStrings } from "./types";

export const en: AppStrings = {
  appTitle: "NRESE Console",
  subtitle:
    "User-facing workspace for exploring, querying, ingesting, and understanding the knowledge graph.",
  examplesTitle: "Guided examples",
  examplesHint:
    "Load a small working example instead of starting from an empty editor. These examples stay close to the actual query, update, tell, and graph-store workflows.",
  applyExample: "Load example",
  runtimeTitle: "Runtime snapshot",
  assistantTitle: "AI query assistant",
  assistantPlaceholder:
    "Describe what you want to learn from the graph, for example: Show the main classes and how many instances they currently have.",
  assistantButton: "Suggest queries",
  useSuggestion: "Use query",
  workbenchTitle: "Knowledge workbench",
  queryTitle: "SPARQL query",
  updateTitle: "SPARQL update",
  tellTitle: "Tell (RDF ingest)",
  graphTitle: "Graph store",
  outputTitle: "Output",
  refresh: "Refresh",
  operatorConsole: "Operator console",
  reasoningPresetTitle: "Reasoning presets",
  reasoningPresetHint:
    "Presets keep runtime opinion outside the implementation. Use them to standardize configuration while keeping the feature policy explicit.",
  reasoningPresetSelectLabel: "Preset preview",
  reasoningPresetActiveLabel: "Active preset",
  reasoningPresetPreviewLabel: "config.toml snippet",
  reasoningPresetCustom: "custom",
  graphDefault: "Default graph",
  graphNamed: "Named graph",
  runQuery: "Run query",
  runUpdate: "Run update",
  runTell: "Ingest RDF",
  loadGraph: "Load graph",
  replaceGraph: "Replace graph",
  mergeGraph: "Merge graph",
  deleteGraph: "Delete graph",
  aiDisabled: "AI suggestions are disabled in this runtime.",
  aiUnavailable: "AI suggestions are currently unavailable.",
  runtimeReady: "Ready",
  runtimeStarting: "Starting",
  queryAcceptLabel: "Result format",
  namedGraphLabel: "Named graph IRI",
  noOutput: "(no response yet)",
  exampleLabels: {
    "overview-query": {
      title: "Top classes by instances",
      description: "Start with a safe overview query that counts the most populated classes.",
    },
    "class-count-query": {
      title: "How many classes exist",
      description: "Use a compact query to verify the current class footprint in the graph.",
    },
    "insert-update": {
      title: "Simple SPARQL INSERT",
      description: "Prefill a minimal update that adds one person and one membership relation.",
    },
    "tell-ingest": {
      title: "Minimal RDF ingest",
      description: "Load a small Turtle example into the tell endpoint without writing SPARQL.",
    },
    "named-graph": {
      title: "Named graph payload",
      description: "Prepare a named-graph write so graph-store operations are easier to understand.",
    },
  },
};
