export const DEFAULT_QUERY = "SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 25";

export const DEFAULT_UPDATE =
  "INSERT DATA { <http://example.com/runtime> <http://example.com/p> <http://example.com/o> . }";

export const DEFAULT_TELL =
  "@prefix ex: <http://example.com/> .\nex:assistantResource ex:hasLabel \"Runtime resource\" .";

export const DEFAULT_GRAPH =
  "@prefix ex: <http://example.com/> .\nex:graphSubject ex:graphPredicate ex:graphObject .";
