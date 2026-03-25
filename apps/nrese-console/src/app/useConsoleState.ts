import { useState } from "react";

import type { AppStrings } from "../i18n/types";
import type { OutputState, QuerySuggestion } from "../lib/types";
import {
  DEFAULT_GRAPH,
  DEFAULT_QUERY,
  DEFAULT_TELL,
  DEFAULT_UPDATE,
} from "./defaults";

export function useConsoleState(strings: AppStrings) {
  const [query, setQuery] = useState(DEFAULT_QUERY);
  const [update, setUpdate] = useState(DEFAULT_UPDATE);
  const [tell, setTell] = useState(DEFAULT_TELL);
  const [graphPayload, setGraphPayload] = useState(DEFAULT_GRAPH);
  const [graphMode, setGraphMode] = useState<"default" | "named">("default");
  const [graphIri, setGraphIri] = useState("http://example.com/runtime-graph");
  const [assistantPrompt, setAssistantPrompt] = useState(strings.assistantPlaceholder);
  const [suggestions, setSuggestions] = useState<QuerySuggestion[]>([]);
  const [output, setOutput] = useState<OutputState>({
    title: strings.outputTitle,
    status: "idle",
    body: strings.noOutput,
  });
  const [assistantBusy, setAssistantBusy] = useState(false);

  return {
    query,
    setQuery,
    update,
    setUpdate,
    tell,
    setTell,
    graphPayload,
    setGraphPayload,
    graphMode,
    setGraphMode,
    graphIri,
    setGraphIri,
    assistantPrompt,
    setAssistantPrompt,
    suggestions,
    setSuggestions,
    output,
    setOutput,
    assistantBusy,
    setAssistantBusy,
  };
}
