import type { AppStrings } from "../i18n/types";
import type { QuerySuggestion } from "../lib/types";

type Props = {
  strings: AppStrings;
  assistantPrompt: string;
  onPromptChange: (value: string) => void;
  onSuggest: () => void;
  suggestions: QuerySuggestion[];
  aiEnabled: boolean;
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
  loading,
  onUseSuggestion,
}: Props) {
  return (
    <section className="panel">
      <div className="panel-header">
        <div>
          <h2>{strings.assistantTitle}</h2>
          <p className="panel-subtitle">
            {aiEnabled ? "Uses the configured server-side AI provider." : strings.aiDisabled}
          </p>
        </div>
      </div>

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
