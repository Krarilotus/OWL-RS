import type { AppStrings } from "../i18n/types";

type GraphMode = "default" | "named";

type Props = {
  strings: AppStrings;
  query: string;
  update: string;
  tell: string;
  graphPayload: string;
  graphMode: GraphMode;
  graphIri: string;
  onQueryChange: (value: string) => void;
  onUpdateChange: (value: string) => void;
  onTellChange: (value: string) => void;
  onGraphPayloadChange: (value: string) => void;
  onGraphModeChange: (value: GraphMode) => void;
  onGraphIriChange: (value: string) => void;
  onRunQuery: () => void;
  onRunUpdate: () => void;
  onRunTell: () => void;
  onReadGraph: () => void;
  onReplaceGraph: () => void;
  onMergeGraph: () => void;
  onDeleteGraph: () => void;
};

export function KnowledgeWorkbench(props: Props) {
  const {
    strings,
    query,
    update,
    tell,
    graphPayload,
    graphMode,
    graphIri,
    onQueryChange,
    onUpdateChange,
    onTellChange,
    onGraphPayloadChange,
    onGraphModeChange,
    onGraphIriChange,
    onRunQuery,
    onRunUpdate,
    onRunTell,
    onReadGraph,
    onReplaceGraph,
    onMergeGraph,
    onDeleteGraph,
  } = props;

  return (
    <section className="panel">
      <div className="panel-header">
        <div>
          <h2>{strings.workbenchTitle}</h2>
          <p className="panel-subtitle">
            Query, update, tell, and graph-store workflows are separated, but share the same output panel.
          </p>
        </div>
      </div>

      <div className="workbench-grid">
        <article className="helper-card">
          <h3>{strings.queryTitle}</h3>
          <div className="field">
            <label htmlFor="query-editor">{strings.queryTitle}</label>
            <textarea id="query-editor" value={query} onChange={(event) => onQueryChange(event.target.value)} />
          </div>
          <div className="button-row">
            <button className="button-primary" onClick={onRunQuery} type="button">
              {strings.runQuery}
            </button>
          </div>
        </article>

        <article className="helper-card">
          <h3>{strings.updateTitle}</h3>
          <div className="field">
            <label htmlFor="update-editor">{strings.updateTitle}</label>
            <textarea id="update-editor" value={update} onChange={(event) => onUpdateChange(event.target.value)} />
          </div>
          <div className="button-row">
            <button className="button-primary" onClick={onRunUpdate} type="button">
              {strings.runUpdate}
            </button>
          </div>
        </article>

        <article className="helper-card">
          <h3>{strings.tellTitle}</h3>
          <div className="field">
            <label htmlFor="tell-editor">{strings.tellTitle}</label>
            <textarea id="tell-editor" value={tell} onChange={(event) => onTellChange(event.target.value)} />
          </div>
          <div className="button-row">
            <button className="button-primary" onClick={onRunTell} type="button">
              {strings.runTell}
            </button>
          </div>
        </article>

        <article className="helper-card">
          <h3>{strings.graphTitle}</h3>
          <div className="field">
            <label htmlFor="graph-mode">Target</label>
            <select
              id="graph-mode"
              value={graphMode}
              onChange={(event) => onGraphModeChange(event.target.value as GraphMode)}
            >
              <option value="default">{strings.graphDefault}</option>
              <option value="named">{strings.graphNamed}</option>
            </select>
          </div>
          <div className="field">
            <label htmlFor="graph-iri">{strings.namedGraphLabel}</label>
            <input
              id="graph-iri"
              value={graphIri}
              onChange={(event) => onGraphIriChange(event.target.value)}
            />
          </div>
          <div className="field">
            <label htmlFor="graph-editor">{strings.graphTitle}</label>
            <textarea
              id="graph-editor"
              value={graphPayload}
              onChange={(event) => onGraphPayloadChange(event.target.value)}
            />
          </div>
          <div className="button-row">
            <button className="button-secondary" onClick={onReadGraph} type="button">
              {strings.loadGraph}
            </button>
            <button className="button-primary" onClick={onReplaceGraph} type="button">
              {strings.replaceGraph}
            </button>
            <button className="button-secondary" onClick={onMergeGraph} type="button">
              {strings.mergeGraph}
            </button>
            <button className="button-secondary" onClick={onDeleteGraph} type="button">
              {strings.deleteGraph}
            </button>
          </div>
        </article>
      </div>
    </section>
  );
}
