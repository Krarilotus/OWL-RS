import type { AppStrings } from "./types";

export const de: AppStrings = {
  appTitle: "NRESE Konsole",
  subtitle:
    "Nutzeroberflaeche zum Erkunden, Abfragen, Einspielen und Verstehen des Wissensgraphen.",
  localeLabel: "Sprache",
  examplesTitle: "Gefuehrte Beispiele",
  examplesHint:
    "Lade ein kleines Arbeitsbeispiel, statt mit einem leeren Editor zu beginnen. Die Beispiele bleiben nah an Query-, Update-, Tell- und Graph-Store-Ablaufen.",
  applyExample: "Beispiel laden",
  runtimeTitle: "Laufzeitstatus",
  assistantTitle: "KI-Abfrageassistent",
  assistantEnabled: "Verwendet den konfigurierten serverseitigen KI-Provider.",
  assistantProviderLabel: "Provider",
  assistantModelLabel: "Modell",
  assistantNoSuggestions: "Fuer den aktuellen Prompt wurden keine Vorschlaege geliefert.",
  assistantPlaceholder:
    "Beschreibe, was du aus dem Graphen wissen willst, zum Beispiel: Zeige die wichtigsten Klassen und wie viele Instanzen sie aktuell haben.",
  assistantButton: "Abfragen vorschlagen",
  useSuggestion: "Abfrage uebernehmen",
  workbenchTitle: "Arbeitsbereich",
  queryTitle: "SPARQL-Abfrage",
  updateTitle: "SPARQL-Update",
  tellTitle: "Tell (RDF-Import)",
  graphTitle: "Graph Store",
  outputTitle: "Ausgabe",
  refresh: "Aktualisieren",
  operatorConsole: "Operator-Konsole",
  reasoningPresetTitle: "Reasoning-Presets",
  reasoningPresetHint:
    "Presets halten Produktentscheidungen aus der Implementierung heraus. So lassen sich Konfigurationen standardisieren, ohne die Feature-Policy zu verstecken.",
  reasoningPresetSelectLabel: "Preset-Vorschau",
  reasoningPresetActiveLabel: "Aktives Preset",
  reasoningPresetPreviewLabel: "config.toml-Snippet",
  reasoningPresetCustom: "custom",
  graphDefault: "Default-Graph",
  graphNamed: "Named Graph",
  runQuery: "Abfrage ausfuehren",
  runUpdate: "Update ausfuehren",
  runTell: "RDF einspielen",
  loadGraph: "Graph laden",
  replaceGraph: "Graph ersetzen",
  mergeGraph: "Graph zusammenfuehren",
  deleteGraph: "Graph loeschen",
  aiDisabled: "KI-Vorschlaege sind in dieser Laufzeit deaktiviert.",
  aiUnavailable: "KI-Vorschlaege sind aktuell nicht verfuegbar.",
  runtimeReady: "Bereit",
  runtimeStarting: "Startet",
  queryAcceptLabel: "Antwortformat",
  namedGraphLabel: "Named-Graph-IRI",
  noOutput: "(noch keine Antwort)",
  exampleLabels: {
    "overview-query": {
      title: "Top-Klassen nach Instanzen",
      description:
        "Starte mit einer sicheren Uebersichtsabfrage fuer die am staerksten belegten Klassen.",
    },
    "class-count-query": {
      title: "Wie viele Klassen existieren",
      description:
        "Nutze eine kompakte Abfrage, um den aktuellen Klassenumfang im Graphen zu pruefen.",
    },
    "insert-update": {
      title: "Einfaches SPARQL-INSERT",
      description:
        "Fuellt ein minimales Update vor, das eine Person und eine Mitgliedschaft anlegt.",
    },
    "tell-ingest": {
      title: "Minimaler RDF-Import",
      description:
        "Lade ein kleines Turtle-Beispiel ueber den Tell-Endpunkt ohne SPARQL-Text.",
    },
    "named-graph": {
      title: "Named-Graph-Payload",
      description:
        "Bereitet einen Named-Graph-Schreibvorgang vor, damit Graph-Store-Aktionen leichter nachvollziehbar werden.",
    },
  },
};
