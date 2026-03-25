import { WORKBENCH_EXAMPLES, type WorkbenchExampleId } from "../app/workbenchExamples";
import type { AppStrings } from "../i18n/types";

type Props = {
  strings: AppStrings;
  onApplyExample: (exampleId: WorkbenchExampleId) => void;
};

export function WorkbenchExamples({ strings, onApplyExample }: Props) {
  return (
    <section className="panel">
      <div className="panel-header">
        <div>
          <h2>{strings.examplesTitle}</h2>
          <p className="panel-subtitle">{strings.examplesHint}</p>
        </div>
      </div>
      <div className="workbench-grid">
        {WORKBENCH_EXAMPLES.map((example) => (
          <article className="helper-card" key={example.id}>
            <h3>{strings.exampleLabels[example.id].title}</h3>
            <p>{strings.exampleLabels[example.id].description}</p>
            <div className="button-row">
              <button
                className="button-secondary"
                onClick={() => onApplyExample(example.id)}
                type="button"
              >
                {strings.applyExample}
              </button>
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
