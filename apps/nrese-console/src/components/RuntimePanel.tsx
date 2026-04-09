import { ReasoningRuntimeInspector } from "./ReasoningRuntimeInspector";
import type { AppStrings } from "../i18n/types";
import type {
  AiStatus,
  Capabilities,
  ReasoningDiagnostics,
  RuntimeSnapshot,
} from "../lib/types";

type Props = {
  strings: AppStrings;
  snapshot?: RuntimeSnapshot;
  capabilities?: Capabilities;
  aiStatus?: AiStatus;
  reasoning?: ReasoningDiagnostics;
  onRefresh: () => void;
};

export function RuntimePanel({
  strings,
  snapshot,
  capabilities,
  aiStatus,
  reasoning,
  onRefresh,
}: Props) {
  const facts = [
    [strings.runtimeRevisionLabel, snapshot?.revision ?? "-"],
    [strings.runtimeQuadsLabel, snapshot?.quad_count ?? "-"],
    [strings.runtimeNamedGraphsLabel, snapshot?.named_graph_count ?? "-"],
    [strings.runtimeDeploymentLabel, snapshot?.deployment_posture ?? "-"],
    [strings.runtimeReasonerLabel, snapshot?.reasoning_profile ?? "-"],
    [strings.runtimePresetLabel, snapshot?.reasoning_preset ?? strings.reasoningPresetCustom],
    [strings.runtimeStoreLabel, snapshot?.store_mode ?? "-"],
    [strings.runtimeDurabilityLabel, snapshot?.durability ?? "-"],
    [
      strings.runtimeAiLabel,
      aiStatus?.enabled
        ? `${aiStatus.provider} / ${aiStatus.model ?? strings.notAvailableLabel}`
        : strings.aiDisabled,
    ],
  ];

  return (
    <section className="panel">
      <div className="panel-header">
        <div>
          <h2>{strings.runtimeTitle}</h2>
          <p className="panel-subtitle">
            {snapshot?.ontology_path ?? strings.runtimeNoOntologyPath}
          </p>
        </div>
        <div className="button-row">
          <button className="button-secondary" onClick={onRefresh} type="button">
            {strings.refresh}
          </button>
          {capabilities?.operator_ui_path ? (
            <a className="button-secondary" href={capabilities.operator_ui_path}>
              {strings.operatorConsole}
            </a>
          ) : null}
        </div>
      </div>

      <div className="fact-grid">
        {facts.map(([label, value]) => (
          <div className="fact-card" key={label}>
            <div className="fact-label">{label}</div>
            <div className="fact-value mono">{String(value)}</div>
          </div>
        ))}
      </div>
      <ReasoningRuntimeInspector
        strings={strings}
        reasoning={reasoning}
      />
    </section>
  );
}
