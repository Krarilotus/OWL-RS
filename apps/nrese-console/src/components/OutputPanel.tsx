import type { AppStrings } from "../i18n/types";
import type { OutputState } from "../lib/types";

type Props = {
  strings: AppStrings;
  output: OutputState;
};

export function OutputPanel({ strings, output }: Props) {
  return (
    <section className="panel output-panel">
      <div className="panel-header">
        <div>
          <h2>{strings.outputTitle}</h2>
          <p className="panel-subtitle">
            {output.title} · <span className="mono">{output.status}</span>
          </p>
        </div>
      </div>
      <pre>{output.body}</pre>
    </section>
  );
}
