import { useRef } from "react";

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
import { describeProblem, type ResponseEnvelope } from "../lib/http";
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
  // The output shows the newest action's result (SYN-72): each action takes the next
  // number when it starts and writes only while no later one has started. Suggestion
  // lists and the assistant's busy mark go by the newest suggestion alone.
  const outputSequence = useRef(0);
  const suggestionSequence = useRef(0);

  async function applyAction(
    title: string,
    operation: Promise<ResponseEnvelope>,
    setOutput: Setter<OutputState>,
    refresh = false,
  ) {
    const ticket = ++outputSequence.current;
    const newest = () => ticket === outputSequence.current;
    let response: ResponseEnvelope;
    try {
      response = await operation;
    } catch (error) {
      if (newest()) {
        setOutput({
          title,
          status: "error",
          body: error instanceof Error ? error.message : String(error),
        });
      }
      return;
    } finally {
      // A finished change refreshes the runtime view whether its answer is shown or not.
      if (refresh) {
        void runtimeRefs.refetchRuntime();
        void runtimeRefs.refetchCapabilities();
      }
    }
    if (!newest()) {
      return;
    }
    const body = response.problem
      ? describeProblem(response.problem)
      : response.body || strings.noOutput;
    // Answers the server doesn't vouch for as complete never read as a plain success.
    const completeness = response.completeness;
    if (response.ok && completeness && completeness.status !== "complete") {
      setOutput({
        title: `${title} (${response.status}, ${completeness.status})`,
        status: "incomplete",
        body: `${completeness.status === "sound-only" ? strings.incompleteAnswer : strings.uncertifiedAnswer} (${completeness.header})\n\n${body}`,
      });
    } else {
      setOutput({
        title: `${title} (${response.status})`,
        status: response.ok ? "success" : "error",
        body,
      });
    }
  }

  async function handleSuggest(state: SuggestionState) {
    const outputTicket = ++outputSequence.current;
    if (!state.aiEnabled) {
      state.setOutput({
        title: strings.assistantTitle,
        status: "error",
        body: strings.aiDisabled,
      });
      return;
    }

    const ticket = ++suggestionSequence.current;
    const newestSuggestion = () => ticket === suggestionSequence.current;
    const showOutput = (output: OutputState) => {
      if (newestSuggestion() && outputTicket === outputSequence.current) {
        state.setOutput(output);
      }
    };
    state.setBusy(true);
    try {
      const response = await getQuerySuggestions({
        prompt: state.prompt,
        locale: state.locale,
        current_query: state.currentQuery,
      });
      if (newestSuggestion()) {
        state.setSuggestions(response.suggestions);
      }
      showOutput({
        title: `${strings.assistantTitle} (${response.provider})`,
        status: "success",
        body: JSON.stringify(response, null, 2),
      });
    } catch (error) {
      showOutput({
        title: strings.assistantTitle,
        status: "error",
        body: error instanceof Error ? error.message : strings.aiUnavailable,
      });
    } finally {
      if (newestSuggestion()) {
        state.setBusy(false);
      }
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
