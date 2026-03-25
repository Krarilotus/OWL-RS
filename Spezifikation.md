# System-Spezifikation: Native Rust Enterprise Semantic Engine (NRESE)

## Leitziel

NRESE verfolgt ein klares Endziel:

> Einen vollstaendig nativen, W3C-konformen Enterprise-Triplestore mit integriertem OWL-DL-Reasoner, zu 100 Prozent in Rust, ohne Python-Bruecken, ohne vermeidbare I/O-Flaschenhaelse und ohne technische Schulden in den Kernschichten.

Diese Datei ist die Master-Spezifikation. Die fachlichen Details wurden in eigenstaendige Teilspezifikationen aufgeteilt, damit Architektur, Storage, Reasoning, API und Betrieb sauber getrennt weiterentwickelt werden koennen.

## Zerlegung in Teilschritte

### Schritt 0: Zielbild und Scope fixieren

- Produktvision, harte Nicht-Ziele und Scope-Grenzen festschreiben
- Explizit festlegen, dass OWL 2 DL das Endziel bleibt
- Lieferphasen definieren, ohne das Endziel abzuschwaechen

Siehe: `docs/spec/00-vision-and-scope.md`

### Schritt 1: Workspace- und Modularchitektur fixieren

- Cargo-Workspace mit klaren Crate-Grenzen
- `nrese-core` als vertragliche Mitte
- Verbot direkter Querverkopplung zwischen Store, Reasoner und Server

Siehe: `docs/spec/01-architecture-workspace.md`

### Schritt 2: Speicherschicht und Snapshot-Semantik definieren

- Oxigraph-basierte oder kompatible Storage-Abstraktion
- Commit-, Snapshot- und Overlay-Modell
- ACID- und MVCC-orientierte Lese-/Schreibsemantik

Siehe: `docs/spec/02-storage-and-transactions.md`

### Schritt 3: Native Reasoning-Architektur definieren

- Rust-native Reasoning-Modi
- TBox-/ABox-Modell
- schrittweiser Ausbau Richtung OWL 2 DL und Tableaux

Siehe: `docs/spec/03-reasoner-and-owl-profile.md`

### Schritt 4: W3C-API und Betriebsgrenzen definieren

- SPARQL Query Protocol
- SPARQL Update Protocol
- Graph Store Protocol
- Auth, TLS, Reverse Proxy, Error-Modell und Observability

Siehe: `docs/spec/04-api-and-protocols.md`

### Schritt 5: Roadmap, Akzeptanzkriterien und Betrieb absichern

- messbare Lieferphasen
- Release-Gates
- Setup-, Upgrade- und Wartungsdokumentation

Siehe: `docs/spec/05-roadmap-and-acceptance.md`

Siehe: `docs/ops/server-setup.md`

Siehe: `docs/ops/server-maintenance.md`

## Architektur-Kernaussagen

- `nrese-core` definiert Typen, Traits und Fehlervertraege.
- `nrese-store` verwaltet persistente RDF-Daten, Snapshots und Commit-Grenzen.
- `nrese-reasoner` berechnet Inferenzen und Konsistenzzustand ohne HTTP- oder Deployment-Wissen.
- `nrese-server` exponiert die W3C-Schnittstellen und orchestriert Store plus Reasoner.

## Aktueller Umsetzungsstand

- Spezifikation in getrennte Fachdokumente zerlegt
- Cargo-Workspace angelegt
- Top-Level-Crates als kompilierbares Grundgeruest erzeugt
- Modultrennung fuer `core`, `store`, `reasoner` und `server` vorbereitet

## Naechster Implementierungsfokus

1. `nrese-core` als vertragliche Kernschicht ausbauen.
2. `nrese-store` mit echter Oxigraph-Adaptergrenze versehen.
3. `nrese-server` mit Konfiguration, Health-Endpoints und Request-Grundgeruest anreichern.
4. `nrese-reasoner` mit sauber deklariertem MVP-Profil und Testfixtures aufbauen.
