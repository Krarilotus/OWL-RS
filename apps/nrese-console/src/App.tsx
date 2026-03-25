import { useMemo } from "react";

import { useConsoleActions } from "./app/useConsoleActions";
import { useConsoleState } from "./app/useConsoleState";
import { useRuntimeData } from "./app/useRuntimeData";
import { WORKBENCH_EXAMPLES } from "./app/workbenchExamples";
import { ConsoleHeader } from "./components/ConsoleHeader";
import { KnowledgeWorkbench } from "./components/KnowledgeWorkbench";
import { OutputPanel } from "./components/OutputPanel";
import { QueryAssistant } from "./components/QueryAssistant";
import { RuntimePanel } from "./components/RuntimePanel";
import { WorkbenchExamples } from "./components/WorkbenchExamples";
import { getStrings } from "./i18n";

export default function App() {
  const strings = useMemo(() => getStrings(navigator.language), []);
  const state = useConsoleState(strings);
  const { runtimeQuery, capabilitiesQuery, aiStatusQuery } = useRuntimeData();
  const actions = useConsoleActions(strings, {
    refetchRuntime: () => runtimeQuery.refetch(),
    refetchCapabilities: () => capabilitiesQuery.refetch(),
  });

  function applyWorkbenchExample(exampleId: (typeof WORKBENCH_EXAMPLES)[number]["id"]) {
    const example = WORKBENCH_EXAMPLES.find((entry) => entry.id === exampleId);
    if (!example) {
      return;
    }
    switch (example.mode) {
      case "query":
        state.setQuery(example.payload);
        break;
      case "update":
        state.setUpdate(example.payload);
        break;
      case "tell":
        state.setTell(example.payload);
        break;
      case "graph":
        state.setGraphPayload(example.payload);
        state.setGraphMode(example.graphMode ?? "default");
        if (example.graphIri) {
          state.setGraphIri(example.graphIri);
        }
        break;
    }
  }

  return (
    <main className="app-shell">
      <ConsoleHeader
        strings={strings}
        statusLabel={
          runtimeQuery.data?.ready ? strings.runtimeReady : strings.runtimeStarting
        }
        ready={Boolean(runtimeQuery.data?.ready)}
      />
      <RuntimePanel
        strings={strings}
        snapshot={runtimeQuery.data}
        capabilities={capabilitiesQuery.data}
        aiStatus={aiStatusQuery.data}
        onRefresh={() => {
          void runtimeQuery.refetch();
          void capabilitiesQuery.refetch();
          void aiStatusQuery.refetch();
        }}
      />
      <QueryAssistant
        strings={strings}
        assistantPrompt={state.assistantPrompt}
        onPromptChange={state.setAssistantPrompt}
        onSuggest={() =>
          void actions.handleSuggest({
            prompt: state.assistantPrompt,
            currentQuery: state.query,
            aiEnabled: Boolean(aiStatusQuery.data?.enabled),
            setBusy: state.setAssistantBusy,
            setSuggestions: state.setSuggestions,
            setOutput: state.setOutput,
          })
        }
        suggestions={state.suggestions}
        aiEnabled={Boolean(aiStatusQuery.data?.enabled)}
        loading={state.assistantBusy}
        onUseSuggestion={state.setQuery}
      />
      <WorkbenchExamples strings={strings} onApplyExample={applyWorkbenchExample} />
      <KnowledgeWorkbench
        strings={strings}
        query={state.query}
        update={state.update}
        tell={state.tell}
        graphPayload={state.graphPayload}
        graphMode={state.graphMode}
        graphIri={state.graphIri}
        onQueryChange={state.setQuery}
        onUpdateChange={state.setUpdate}
        onTellChange={state.setTell}
        onGraphPayloadChange={state.setGraphPayload}
        onGraphModeChange={state.setGraphMode}
        onGraphIriChange={state.setGraphIri}
        onRunQuery={() =>
          void actions.applyAction(
            "query",
            actions.runQuery(state.query, "application/sparql-results+json"),
            state.setOutput,
          )
        }
        onRunUpdate={() =>
          void actions.applyAction(
            "update",
            actions.runUpdate(state.update),
            state.setOutput,
            true,
          )
        }
        onRunTell={() =>
          void actions.applyAction(
            "tell",
            actions.runTell(state.tell, state.graphMode, state.graphIri),
            state.setOutput,
            true,
          )
        }
        onReadGraph={() =>
          void actions.applyAction(
            "graph",
            actions.readGraph(state.graphMode, state.graphIri),
            state.setOutput,
          )
        }
        onReplaceGraph={() =>
          void actions.applyAction(
            "graph replace",
            actions.writeGraph("PUT", state.graphPayload, state.graphMode, state.graphIri),
            state.setOutput,
            true,
          )
        }
        onMergeGraph={() =>
          void actions.applyAction(
            "graph merge",
            actions.writeGraph("POST", state.graphPayload, state.graphMode, state.graphIri),
            state.setOutput,
            true,
          )
        }
        onDeleteGraph={() =>
          void actions.applyAction(
            "graph delete",
            actions.deleteGraph(state.graphMode, state.graphIri),
            state.setOutput,
            true,
          )
        }
      />
      <OutputPanel strings={strings} output={state.output} />
    </main>
  );
}
