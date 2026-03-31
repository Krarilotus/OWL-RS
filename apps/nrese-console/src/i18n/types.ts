export type AppStrings = {
  appTitle: string;
  subtitle: string;
  localeLabel: string;
  examplesTitle: string;
  examplesHint: string;
  applyExample: string;
  runtimeTitle: string;
  assistantTitle: string;
  assistantEnabled: string;
  assistantProviderLabel: string;
  assistantModelLabel: string;
  assistantNoSuggestions: string;
  assistantPlaceholder: string;
  assistantButton: string;
  useSuggestion: string;
  workbenchTitle: string;
  queryTitle: string;
  updateTitle: string;
  tellTitle: string;
  graphTitle: string;
  outputTitle: string;
  refresh: string;
  operatorConsole: string;
  reasoningPresetTitle: string;
  reasoningPresetHint: string;
  reasoningPresetSelectLabel: string;
  reasoningPresetActiveLabel: string;
  reasoningPresetPreviewLabel: string;
  reasoningPresetCustom: string;
  graphDefault: string;
  graphNamed: string;
  runQuery: string;
  runUpdate: string;
  runTell: string;
  loadGraph: string;
  replaceGraph: string;
  mergeGraph: string;
  deleteGraph: string;
  aiDisabled: string;
  aiUnavailable: string;
  runtimeReady: string;
  runtimeStarting: string;
  queryAcceptLabel: string;
  namedGraphLabel: string;
  noOutput: string;
  exampleLabels: Record<
    "overview-query" | "class-count-query" | "insert-update" | "tell-ingest" | "named-graph",
    {
      title: string;
      description: string;
    }
  >;
};
