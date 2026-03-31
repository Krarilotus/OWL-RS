import type { AppStrings } from "../i18n/types";
import type { ReasoningDiagnostics } from "../lib/types";

type Props = {
  strings: AppStrings;
  reasoning?: ReasoningDiagnostics;
};

function buildConfigSnippet(reasoning?: ReasoningDiagnostics): string {
  const policy = reasoning?.configured_policy;
  if (!policy) {
    return "[reasoner]\nmode = \"disabled\"";
  }

  const featureLines = policy.feature_modes
    .map(({ feature, mode }) => `${feature} = "${mode}"`)
    .join("\n");

  return [
    "[reasoner]",
    `mode = "${reasoning?.mode ?? "rules-mvp"}"`,
    "",
    "[reasoner.rules_mvp]",
    `preset = "${policy.preset}"`,
    featureLines,
    `unsupported_constructs = "${policy.unsupported_constructs}"`,
  ]
    .filter(Boolean)
    .join("\n");
}

export function ReasoningRuntimeInspector({ strings, reasoning }: Props) {
  const policy = reasoning?.configured_policy;
  const lastRun = reasoning?.last_run;
  const configSnippet = buildConfigSnippet(reasoning);

  return (
    <div className="preset-panel">
      <div className="preset-panel-header">
        <h3>{strings.reasoningPresetTitle}</h3>
        <p className="panel-subtitle">{strings.reasoningPresetHint}</p>
      </div>

      <div className="fact-grid">
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningPresetActiveLabel}</div>
          <div className="fact-value mono">
            {policy?.preset ?? strings.reasoningPolicyUnavailable}
          </div>
        </div>
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningPresetAvailableLabel}</div>
          <div className="fact-value mono">
            {(policy?.available_presets ?? []).join(", ") || strings.reasoningPolicyUnavailable}
          </div>
        </div>
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningUnsupportedLabel}</div>
          <div className="fact-value mono">
            {policy?.unsupported_constructs ?? strings.reasoningPolicyUnavailable}
          </div>
        </div>
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningLastRunLabel}</div>
          <div className="fact-value mono">
            {lastRun
              ? `${lastRun.status} @ r${lastRun.revision}`
              : strings.reasoningPolicyUnavailable}
          </div>
        </div>
      </div>

      <div className="fact-grid">
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningCacheExecutionLabel}</div>
          <div className="fact-value mono">
            {lastRun
              ? `${lastRun.cache.execution_cache_entries}/${lastRun.cache.execution_cache_capacity} (${lastRun.cache.execution_cache_hit ? strings.yesLabel : strings.noLabel})`
              : strings.reasoningPolicyUnavailable}
          </div>
        </div>
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningCacheSchemaLabel}</div>
          <div className="fact-value mono">
            {lastRun
              ? `${lastRun.cache.schema_cache_entries}/${lastRun.cache.schema_cache_capacity} (${lastRun.cache.schema_cache_hit ? strings.yesLabel : strings.noLabel})`
              : strings.reasoningPolicyUnavailable}
          </div>
        </div>
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningCacheExecutionTotalsLabel}</div>
          <div className="fact-value mono">
            {lastRun
              ? `${lastRun.cache.execution_cache_hits_total} / ${lastRun.cache.execution_cache_misses_total}`
              : strings.reasoningPolicyUnavailable}
          </div>
        </div>
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningCacheSchemaTotalsLabel}</div>
          <div className="fact-value mono">
            {lastRun
              ? `${lastRun.cache.schema_cache_hits_total} / ${lastRun.cache.schema_cache_misses_total}`
              : strings.reasoningPolicyUnavailable}
          </div>
        </div>
      </div>

      <div className="field">
        <label>{strings.reasoningFeatureModesLabel}</label>
        <div className="fact-grid">
          {(policy?.feature_modes ?? []).map((feature) => (
            <div className="fact-card" key={feature.feature}>
              <div className="fact-label">{feature.feature}</div>
              <div className="fact-value mono">{feature.mode}</div>
            </div>
          ))}
          {(policy?.feature_modes ?? []).length === 0 ? (
            <div className="fact-card">
              <div className="fact-value mono">{strings.reasoningPolicyUnavailable}</div>
            </div>
          ) : null}
        </div>
      </div>

      <div className="field">
        <label>{strings.reasoningConfigSnippetLabel}</label>
        <pre className="code-inline-preview">{configSnippet}</pre>
      </div>
    </div>
  );
}
