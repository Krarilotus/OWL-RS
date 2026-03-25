import { useMemo, useState } from "react";

import type { AppStrings } from "../i18n/types";

type Props = {
  strings: AppStrings;
  activePreset?: string | null;
  availablePresets: string[];
};

export function ReasoningPresetPanel({
  strings,
  activePreset,
  availablePresets,
}: Props) {
  const defaultPreset = activePreset ?? availablePresets[0] ?? "bounded-owl";
  const [selectedPreset, setSelectedPreset] = useState(defaultPreset);
  const configSnippet = useMemo(
    () =>
      `[reasoner]\nmode = "rules-mvp"\n\n[reasoner.rules_mvp]\npreset = "${selectedPreset}"`,
    [selectedPreset],
  );

  return (
    <div className="preset-panel">
      <div className="preset-panel-header">
        <h3>{strings.reasoningPresetTitle}</h3>
        <p className="panel-subtitle">{strings.reasoningPresetHint}</p>
      </div>
      <div className="field">
        <label htmlFor="reasoning-preset-select">
          {strings.reasoningPresetSelectLabel}
        </label>
        <select
          id="reasoning-preset-select"
          value={selectedPreset}
          onChange={(event) => setSelectedPreset(event.target.value)}
        >
          {availablePresets.map((preset) => (
            <option key={preset} value={preset}>
              {preset}
            </option>
          ))}
        </select>
      </div>
      <div className="fact-grid">
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningPresetActiveLabel}</div>
          <div className="fact-value mono">
            {activePreset ?? strings.reasoningPresetCustom}
          </div>
        </div>
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningPresetPreviewLabel}</div>
          <pre className="code-inline-preview">{configSnippet}</pre>
        </div>
      </div>
    </div>
  );
}
