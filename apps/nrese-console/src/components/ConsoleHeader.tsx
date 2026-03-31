import type { AppStrings } from "../i18n/types";
import { SUPPORTED_LOCALES, type SupportedLocale } from "../i18n";

type Props = {
  strings: AppStrings;
  statusLabel: string;
  ready: boolean;
  locale: SupportedLocale;
  onLocaleChange: (locale: SupportedLocale) => void;
};

export function ConsoleHeader({
  strings,
  statusLabel,
  ready,
  locale,
  onLocaleChange,
}: Props) {
  return (
    <section className="hero">
      <div className="hero-row">
        <div>
          <h1>{strings.appTitle}</h1>
          <p className="panel-subtitle">{strings.subtitle}</p>
        </div>
        <div>
          <div className="field">
            <label htmlFor="locale-select">{strings.localeLabel}</label>
            <select
              id="locale-select"
              value={locale}
              onChange={(event) => onLocaleChange(event.target.value as SupportedLocale)}
            >
              {SUPPORTED_LOCALES.map((entry) => (
                <option key={entry.code} value={entry.code}>
                  {entry.label}
                </option>
              ))}
            </select>
          </div>
          <div className={`pill ${ready ? "pill--ready" : "pill--starting"}`}>
            {statusLabel}
          </div>
        </div>
      </div>
    </section>
  );
}
