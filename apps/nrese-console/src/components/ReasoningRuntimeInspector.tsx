import type { AppStrings } from "../i18n/types";
import type { ReasoningDiagnostics } from "../lib/types";
import {
  buildReasoningConfigSnippet,
  sortReasoningCapabilities,
} from "../app/reasoningPresentation";

type Props = {
  strings: AppStrings;
  reasoning?: ReasoningDiagnostics;
};

export function ReasoningRuntimeInspector({ strings, reasoning }: Props) {
  const lastRun = reasoning?.last_run;
  const configSnippet = buildReasoningConfigSnippet(reasoning);
  const capabilities = sortReasoningCapabilities(reasoning?.capabilities);
  const unavailable = strings.reasoningPolicyUnavailable;

  return (
    <div className="preset-panel">
      <div className="preset-panel-header">
        <h3>{strings.reasoningPresetTitle}</h3>
        <p className="panel-subtitle">{strings.reasoningPresetHint}</p>
      </div>

      <div className="fact-grid">
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningPresetActiveLabel}</div>
          <div className="fact-value mono">{reasoning?.mode ?? unavailable}</div>
        </div>
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningPresetTierLabel}</div>
          <div className="fact-value mono">{reasoning?.profile ?? unavailable}</div>
        </div>
        <div className="fact-card">
          <div className="fact-label">{strings.reasoningLastRunLabel}</div>
          <div className="fact-value mono">
            {lastRun ? `${lastRun.status} @ r${lastRun.revision}` : unavailable}
          </div>
        </div>
      </div>

      <div className="field">
        <label>{strings.reasoningRunTitle}</label>
        <div className="fact-grid">
          <div className="fact-card">
            <div className="fact-label">{strings.reasoningRunRulesetLabel}</div>
            <div className="fact-value mono">{lastRun ? lastRun.ruleset : unavailable}</div>
          </div>
          <div className="fact-card">
            <div className="fact-label">{strings.reasoningRunInferredLabel}</div>
            <div className="fact-value mono">
              {lastRun ? lastRun.inferred_triples : unavailable}
            </div>
          </div>
          <div className="fact-card">
            <div className="fact-label">{strings.reasoningRunChangesLabel}</div>
            <div className="fact-value mono">
              {lastRun
                ? `+${lastRun.inferred_inserted} / -${lastRun.inferred_deleted}`
                : unavailable}
            </div>
          </div>
          <div className="fact-card">
            <div className="fact-label">{strings.reasoningRunViolationsLabel}</div>
            <div className="fact-value mono">
              {lastRun ? lastRun.consistency_violations : unavailable}
            </div>
          </div>
          <div className="fact-card">
            <div className="fact-label">{strings.reasoningRunRoundsLabel}</div>
            <div className="fact-value mono">{lastRun ? lastRun.rounds : unavailable}</div>
          </div>
          <div className="fact-card">
            <div className="fact-label">{strings.reasoningRunTimeLabel}</div>
            <div className="fact-value mono">
              {lastRun ? `${(lastRun.elapsed_micros / 1000).toFixed(2)} ms` : unavailable}
            </div>
          </div>
        </div>
      </div>

      <div className="field">
        <label>{strings.reasoningCapabilitiesLabel}</label>
        <div className="fact-grid">
          {capabilities.map((capability) => (
            <div className="fact-card" key={capability.feature}>
              <div className="fact-label">{capability.feature}</div>
              <div className="fact-value mono">
                {`${strings.reasoningCapabilityMaturityLabel}: ${capability.maturity}`}
              </div>
              <div className="fact-value mono">
                {`${strings.reasoningCapabilityDefaultLabel}: ${capability.enabled_by_default ? strings.yesLabel : strings.noLabel}`}
              </div>
            </div>
          ))}
          {capabilities.length === 0 ? (
            <div className="fact-card">
              <div className="fact-value mono">{unavailable}</div>
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
