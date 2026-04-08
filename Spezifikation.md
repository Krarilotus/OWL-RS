# NRESE Spezifikations-Index

Diese Datei ist der kompakte Einstieg in die Spezifikation.
Sie ist kein zweites Detail-PRD neben `docs/spec/`, sondern der Dokumentenvertrag dafuer:

- welche Spec-Datei welchen Concern besitzt
- wo der aktuelle Status verbindlich gepflegt wird
- was als Naechstes geschlossen werden muss

## Kanonische Spec-Quellen

Es gibt im Repository genau einen eigentlichen Gap-Tracker:

- [docs/spec/06-fuseki-replacement-gap-matrix.md](docs/spec/06-fuseki-replacement-gap-matrix.md)
  - verbindliche Statusquelle fuer `done | partial | missing`
  - verbindliche Replacement-Gates

Die anderen Steuerungsdokumente haben bewusst andere Rollen:

- [docs/spec/05-roadmap-and-acceptance.md](docs/spec/05-roadmap-and-acceptance.md)
  - Release-Stufen, Akzeptanzkriterien, Governance-Regeln
- [docs/spec/07-replacement-implementation-plan.md](docs/spec/07-replacement-implementation-plan.md)
  - aktive Umsetzungsreihenfolge, Tracks, Ownership, Reactor-Schritte

Die fachlichen Detail-Sources-of-Truth bleiben getrennt:

- [docs/spec/00-vision-and-scope.md](docs/spec/00-vision-and-scope.md)
  - Vision, Scope, Guardrails
- [docs/spec/01-architecture-workspace.md](docs/spec/01-architecture-workspace.md)
  - Crate-Grenzen und Architekturregeln
- [docs/spec/02-storage-and-transactions.md](docs/spec/02-storage-and-transactions.md)
  - Storage-, Snapshot- und Transaktionsvertraege
- [docs/spec/03-reasoner-and-owl-profile.md](docs/spec/03-reasoner-and-owl-profile.md)
  - Reasoner-Profile, Supportgrenzen, Diagnostik
- [docs/spec/04-api-and-protocols.md](docs/spec/04-api-and-protocols.md)
  - HTTP-, SPARQL-, Tell-/Ask-/Services-Vertraege

Ops-Dokumente sind operative Sources of Truth, nicht Spec-Ersatz:

- [docs/ops/config-reference.md](docs/ops/config-reference.md)
- [docs/ops/server-setup.md](docs/ops/server-setup.md)
- [docs/ops/benchmark-and-conformance.md](docs/ops/benchmark-and-conformance.md)
- [docs/ops/backup-restore-drills.md](docs/ops/backup-restore-drills.md)

## Aktueller Kurzstatus

Aktuell ist NRESE ein fortgeschrittener Pilot mit substanzieller Server-, Store-, Reasoner- und Frontend-Basis, aber noch kein vollstaendig belegter Fuseki-Ersatz.

Verbindlicher Kurzstand je Track:

- Protokoll- und Fuseki-Paritaet: `partial`
  - Query, Update, Graph Store, Tell, Service Description, Harness, Workload Packs und eine wiederverwendbare Connection-Profile-Registry fuer gesicherte Live-Parity existieren
  - kataloggetriebene `pack-matrix`-Laeufe koennen jetzt auch gezielt eine einzelne offizielle Ontologie selektieren, statt nur Tier- oder Metadatenfilter zu verwenden
  - Live-Parity-Packs koennen jetzt explizit als `compat-only` laufen, wenn bestehende Deployments ohne Seed-/Bench-Nebenwirkungen verglichen werden sollen
  - lokale Side-by-Side-Evidenz gegen Apache Jena Fuseki 6.0.0 liegt jetzt fuer FOAF, ORG und SKOS auf dem normalen `pack-report.json`-/`pack-matrix-report.json`-Pfad vor
  - echte Live-Evidenz gegen die Ziel-Fuseki-Umgebung fehlt noch
- Reasoner: `partial`
  - `rules-mvp` ist real und deckt RDFS plus einen bounded OWL-Slice ab
  - breitere EL/RL-Abdeckung, tiefere Justifications und DL-Pfad fehlen
- Persistenz und Recovery: `partial`
  - durable Pfad, Backup/Restore und atomare Fehlerpfade existieren
  - Crash-/Restart-/Drill-Evidenz ist noch offen
- Security und Hardening: `partial`
  - `bearer-static`, bounded `bearer-jwt`, bounded `oidc-introspection`, bounded proxy-terminated `mtls`, Limits und Rate-Limits existieren
  - produktionsnahe Härtungs- und Rollout-Evidenz fehlt noch
- Frontend und Operator-Flaechen: `partial`
  - `/console` und `/ops` existieren, inklusive AI-Assistent, i18n und Runtime-/Reasoning-Sicht
  - reale Workflow-Evidenz und weitere UX-Haertung fehlen
- Performance- und Compatibility-Evidenz: `partial`
  - Harness, Ontologiekatalog, Reports und Pack-Index existieren
  - echte Ziel-Workloads, RAM/CPU/Startup/Reasoning-Kosten und CI-Gates fehlen

## Naechste priorisierte Luecken

Die naechsten Replacement-Bloecke sind:

1. echte secured Side-by-Side-Paritaet gegen Fuseki
   - Timeout-, Auth-, Error- und Workload-Packs gegen die echte Zielumgebung, gebunden an eine konkrete Live-Connection-Profile-Auswahl
2. breitere Reasoner-Abdeckung auf dem bestehenden modularen Pfad
   - ohne Supportgrenzen zu verwischen oder impliziten Code einzubauen
3. durable Recovery- und Drill-Evidenz
4. CI-faehige Benchmark-/Conformance-Gates
5. weitere Frontend-Workflow-Haertung auf Basis der echten Server-/Reasoning-Flaechen

## Dokumentationsregeln

Wenn sich Verhalten aendert, muessen die Spec-Dateien gezielt aktualisiert werden:

- Verhaltensaenderung je Concern:
  - die owning Spec-Datei unter `docs/spec/00-04`
- neuer/reiferer Replacement-Status:
  - [docs/spec/06-fuseki-replacement-gap-matrix.md](docs/spec/06-fuseki-replacement-gap-matrix.md)
- neue Reihenfolge, neuer Track oder neuer Reactor-Kandidat:
  - [docs/spec/07-replacement-implementation-plan.md](docs/spec/07-replacement-implementation-plan.md)
- neue Abnahme-/Release-Aussage:
  - [docs/spec/05-roadmap-and-acceptance.md](docs/spec/05-roadmap-and-acceptance.md)
- neue operative Knobs, Pack-Formate oder Betriebsablaeufe:
  - entsprechendes Dokument unter `docs/ops/`

Wenn Dokumente widersprechen, gilt:

1. owning Fach-Spec fuer Verhalten
2. `06` fuer Status
3. `07` fuer aktive Umsetzungsplanung
4. `05` fuer Release- und Acceptance-Gates

## Lesereihenfolge

Fuer neue Mitarbeitende ist die kuerzeste sinnvolle Reihenfolge:

1. diese Datei
2. [docs/spec/06-fuseki-replacement-gap-matrix.md](docs/spec/06-fuseki-replacement-gap-matrix.md)
3. [docs/spec/07-replacement-implementation-plan.md](docs/spec/07-replacement-implementation-plan.md)
4. die owning Detail-Spec fuer den jeweiligen Concern
