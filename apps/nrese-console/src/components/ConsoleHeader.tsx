import type { AppStrings } from "../i18n/types";

type Props = {
  strings: AppStrings;
  statusLabel: string;
  ready: boolean;
};

export function ConsoleHeader({ strings, statusLabel, ready }: Props) {
  return (
    <section className="hero">
      <div className="hero-row">
        <div>
          <h1>{strings.appTitle}</h1>
          <p className="panel-subtitle">{strings.subtitle}</p>
        </div>
        <div>
          <div className={`pill ${ready ? "pill--ready" : "pill--starting"}`}>
            {statusLabel}
          </div>
        </div>
      </div>
    </section>
  );
}
