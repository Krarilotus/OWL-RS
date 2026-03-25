import { ReasoningPresetPanel } from "./ReasoningPresetPanel";
import type { AppStrings } from "../i18n/types";
import type { AiStatus, Capabilities, RuntimeSnapshot } from "../lib/types";

type Props = {
  strings: AppStrings;
  snapshot?: RuntimeSnapshot;
  capabilities?: Capabilities;
  aiStatus?: AiStatus;
  onRefresh: () => void;
};

export function RuntimePanel({ strings, snapshot, capabilities, aiStatus, onRefresh }: Props) {
  const facts = [
    ["Revision", snapshot?.revision ?? "-"],
    ["Quads", snapshot?.quad_count ?? "-"],
    ["Named Graphs", snapshot?.named_graph_count ?? "-"],
    ["Reasoner", snapshot?.reasoning_profile ?? "-"],
    ["Preset", snapshot?.reasoning_preset ?? "custom"],
    ["Store", snapshot?.store_mode ?? "-"],
    ["Durability", snapshot?.durability ?? "-"],
    ["AI", aiStatus?.enabled ? `${aiStatus.provider} / ${aiStatus.model ?? "-"}` : "disabled"],
  ];

  return (
    <section className="panel">
      <div className="panel-header">
        <div>
          <h2>{strings.runtimeTitle}</h2>
          <p className="panel-subtitle">
            {snapshot?.ontology_path ?? "No explicit ontology preload path configured."}
          </p>
        </div>
        <div className="button-row">
          <button className="button-secondary" onClick={onRefresh} type="button">
            {strings.refresh}
          </button>
          <a className="button-secondary" href={capabilities?.operator_ui_path ?? "/ops"}>
            {strings.operatorConsole}
          </a>
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
      <ReasoningPresetPanel
        strings={strings}
        activePreset={snapshot?.reasoning_preset ?? capabilities?.reasoning_preset}
        availablePresets={capabilities?.available_reasoning_presets ?? []}
      />
    </section>
  );
}
