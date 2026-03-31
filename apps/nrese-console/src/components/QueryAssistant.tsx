import type { AppStrings } from "../i18n/types";
import type { AiStatus, QuerySuggestion } from "../lib/types";

type Props = {
  strings: AppStrings;
  assistantPrompt: string;
  onPromptChange: (value: string) => void;
  onSuggest: () => void;
  suggestions: QuerySuggestion[];
  aiEnabled: boolean;
  aiStatus?: AiStatus;
  loading: boolean;
  onUseSuggestion: (query: string) => void;
};

export function QueryAssistant({
  strings,
  assistantPrompt,
  onPromptChange,
  onSuggest,
  suggestions,
  aiEnabled,
  aiStatus,
  loading,
  onUseSuggestion,
}: Props) {
  return (
    <section className="panel">
      <div className="panel-header">
        <div>
          <h2>{strings.assistantTitle}</h2>
          <p className="panel-subtitle">
            {aiEnabled ? strings.assistantEnabled : strings.aiDisabled}
          </p>
        </div>
      </div>

      {aiStatus?.enabled ? (
        <div className="workbench-grid">
          <article className="helper-card">
            <h3>{strings.assistantProviderLabel}</h3>
            <p>{aiStatus.provider}</p>
          </article>
          <article className="helper-card">
            <h3>{strings.assistantModelLabel}</h3>
            <p>{aiStatus.model ?? strings.notAvailableLabel}</p>
          </article>
        </div>
      ) : null}

      <div className="field">
        <label htmlFor="assistant-prompt">{strings.assistantTitle}</label>
        <textarea
          id="assistant-prompt"
          placeholder={strings.assistantPlaceholder}
          value={assistantPrompt}
          onChange={(event) => onPromptChange(event.target.value)}
        />
      </div>

      <div className="button-row">
        <button
          className="button-primary"
          onClick={onSuggest}
          disabled={!aiEnabled || loading}
          type="button"
        >
          {strings.assistantButton}
        </button>
      </div>

      <div className="suggestion-grid">
        {suggestions.length === 0 && !loading ? <p>{strings.assistantNoSuggestions}</p> : null}
        {suggestions.map((suggestion) => (
          <article className="suggestion-card" key={suggestion.title}>
            <h3>{suggestion.title}</h3>
            <p>{suggestion.explanation}</p>
            <pre>{suggestion.sparql}</pre>
            <div className="button-row">
              <button
                className="button-secondary"
                onClick={() => onUseSuggestion(suggestion.sparql)}
                type="button"
              >
                {strings.useSuggestion}
              </button>
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
