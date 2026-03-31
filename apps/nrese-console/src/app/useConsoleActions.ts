import type { AppStrings } from "../i18n/types";
import {
  deleteGraph,
  getQuerySuggestions,
  readGraph,
  runQuery,
  runTell,
  runUpdate,
  writeGraph,
} from "../lib/api";
import type { OutputState, QuerySuggestion } from "../lib/types";

type Setter<T> = (value: T) => void;

type QueryRefetch = () => void | Promise<unknown>;

type RuntimeRefs = {
  refetchRuntime: QueryRefetch;
  refetchCapabilities: QueryRefetch;
};

type SuggestionState = {
  prompt: string;
  currentQuery: string;
  locale: string;
  aiEnabled: boolean;
  setBusy: Setter<boolean>;
  setSuggestions: Setter<QuerySuggestion[]>;
  setOutput: Setter<OutputState>;
};

export function useConsoleActions(strings: AppStrings, runtimeRefs: RuntimeRefs) {
  async function applyAction(
    title: string,
    operation: Promise<{ ok: boolean; status: number; body: string }>,
    setOutput: Setter<OutputState>,
    refresh = false,
  ) {
    const response = await operation;
    setOutput({
      title: `${title} (${response.status})`,
      status: response.ok ? "success" : "error",
      body: response.body || strings.noOutput,
    });
    if (refresh) {
      void runtimeRefs.refetchRuntime();
      void runtimeRefs.refetchCapabilities();
    }
  }

  async function handleSuggest(state: SuggestionState) {
    if (!state.aiEnabled) {
      state.setOutput({
        title: strings.assistantTitle,
        status: "error",
        body: strings.aiDisabled,
      });
      return;
    }

    state.setBusy(true);
    try {
      const response = await getQuerySuggestions({
        prompt: state.prompt,
        locale: state.locale,
        current_query: state.currentQuery,
      });
      state.setSuggestions(response.suggestions);
      state.setOutput({
        title: `${strings.assistantTitle} (${response.provider})`,
        status: "success",
        body: JSON.stringify(response, null, 2),
      });
    } catch (error) {
      state.setOutput({
        title: strings.assistantTitle,
        status: "error",
        body: error instanceof Error ? error.message : strings.aiUnavailable,
      });
    } finally {
      state.setBusy(false);
    }
  }

  return {
    applyAction,
    handleSuggest,
    runQuery,
    runUpdate,
    runTell,
    readGraph,
    writeGraph,
    deleteGraph,
  };
}
