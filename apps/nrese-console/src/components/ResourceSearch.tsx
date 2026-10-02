import { useEffect, useState } from "react";

import type { AppStrings } from "../i18n/types";
import { autocomplete } from "../lib/api";
import type { ResourceSuggestion } from "../lib/types";

type Props = {
  strings: AppStrings;
  onDescribe: (iri: string) => void;
};

/** Finds resources by the beginning of their labels' or local names' words. */
export function ResourceSearch({ strings, onDescribe }: Props) {
  const [typed, setTyped] = useState("");
  const [suggestions, setSuggestions] = useState<ResourceSuggestion[]>([]);
  const [failed, setFailed] = useState(false);
  // The text the suggestions answer.
  const [answered, setAnswered] = useState("");

  useEffect(() => {
    const text = typed.trim();
    if (!text) {
      setSuggestions([]);
      return;
    }
    // Ask once typing pauses; an answer to older text is dropped.
    let current = true;
    const timer = window.setTimeout(() => {
      autocomplete(text)
        .then((response) => {
          if (current) {
            setSuggestions(response.suggestions);
            setAnswered(text);
            setFailed(false);
          }
        })
        .catch(() => {
          if (current) {
            setSuggestions([]);
            setFailed(true);
          }
        });
    }, 150);
    return () => {
      current = false;
      window.clearTimeout(timer);
    };
  }, [typed]);

  return (
    <section className="panel">
      <div className="panel-header">
        <div>
          <h2>{strings.searchTitle}</h2>
          <p className="panel-subtitle">{strings.searchHint}</p>
        </div>
      </div>
      <div className="field">
        <label htmlFor="resource-search">{strings.searchTitle}</label>
        <input
          id="resource-search"
          placeholder={strings.searchPlaceholder}
          value={typed}
          onChange={(event) => setTyped(event.target.value)}
        />
      </div>
      {failed ? <p>{strings.searchUnavailable}</p> : null}
      {typed.trim() && answered === typed.trim() && !failed && suggestions.length === 0 ? (
        <p>{strings.searchNoResults}</p>
      ) : null}
      <div className="suggestion-grid">
        {suggestions.map((suggestion) => (
          <article className="suggestion-card" key={suggestion.iri}>
            <h3>{suggestion.label ?? suggestion.iri}</h3>
            {suggestion.label ? <p>{suggestion.iri}</p> : null}
            <div className="button-row">
              <button
                className="button-secondary"
                onClick={() => onDescribe(suggestion.iri)}
                type="button"
              >
                {strings.describeResource}
              </button>
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
