export type GraphMode = "default" | "named";

export type WorkbenchExampleId =
  | "overview-query"
  | "class-count-query"
  | "insert-update"
  | "tell-ingest"
  | "named-graph";

export type WorkbenchExample = {
  id: WorkbenchExampleId;
  mode: "query" | "update" | "tell" | "graph";
  graphMode?: GraphMode;
  graphIri?: string;
  payload: string;
};

export const WORKBENCH_EXAMPLES: readonly WorkbenchExample[] = [
  {
    id: "overview-query",
    mode: "query",
    payload: `SELECT ?class (COUNT(?instance) AS ?instances)
WHERE {
  ?instance a ?class .
}
GROUP BY ?class
ORDER BY DESC(?instances)
LIMIT 10`,
  },
  {
    id: "class-count-query",
    mode: "query",
    payload: `SELECT (COUNT(DISTINCT ?class) AS ?classes)
WHERE {
  ?s a ?class .
}`,
  },
  {
    id: "insert-update",
    mode: "update",
    payload: `PREFIX ex: <http://example.com/>
INSERT DATA {
  ex:alice a ex:Person ;
    ex:memberOf ex:engineering .
}`,
  },
  {
    id: "tell-ingest",
    mode: "tell",
    payload: `@prefix ex: <http://example.com/> .

ex:bob a ex:Person ;
  ex:memberOf ex:research .`,
  },
  {
    id: "named-graph",
    mode: "graph",
    graphMode: "named",
    graphIri: "http://example.com/graphs/demo",
    payload: `@prefix ex: <http://example.com/> .

ex:system ex:status "ready" .`,
  },
];
