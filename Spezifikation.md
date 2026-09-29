# NRESE Spezifikations-Index

Kompakter Einstieg: welches Dokument welchen Concern besitzt und wo der verbindliche Stand steht.

## Ziel

NRESE wird eine RDF-Datenbank, die **wie QLever liest und Daten wie GraphDB verwaltet**: eigene Storage-Engine, materialisiertes RDFS/OWL-2-RL-Reasoning, SHACL beim Commit, Volltextsuche, RDF4J-Protokoll. Fuseki ist kein Paritätsziel mehr ([ADR-0004](docs/adr/0004-parity-targets-qlever-graphdb.md)). Hauptnutzer: ResearchSpace und der Datamodel-Workflow.

## Verbindliche Quellen

| Frage | Dokument |
|---|---|
| Wer besitzt was, in welcher Schicht wird was gefixt? | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| Warum wurde etwas so entschieden? | [docs/adr/](docs/adr/) |
| Was wird in welcher Reihenfolge gebaut? | [docs/ROADMAP.md](docs/ROADMAP.md) |
| Wie weit sind wir gegenüber QLever und GraphDB? | [docs/spec/06-target-capability-matrix.md](docs/spec/06-target-capability-matrix.md) |
| Was das Reasoning berechnet (Regeln, Auslassungen, Graph-Scope, Konsistenz) | [docs/spec/reasoning-semantics.md](docs/spec/reasoning-semantics.md) |
| Verhalten der v1-Implementierung (historisch) | [docs/spec/02](docs/spec/02-storage-and-transactions.md), [03](docs/spec/03-reasoner-and-owl-profile.md), [04](docs/spec/04-api-and-protocols.md) |
| Betrieb und Konfiguration | [docs/ops/](docs/ops/), vor allem [config-reference.md](docs/ops/config-reference.md) |
| Code-Regeln | [docs/dev/code-structure-guidelines.md](docs/dev/code-structure-guidelines.md) |

Überholte Dokumente (Fuseki-Gap-Matrix, alter Umsetzungsplan) liegen in [docs/archive/](docs/archive/).

## Konfliktregel

Wenn Dokumente sich widersprechen: ARCHITECTURE vor ADR vor Fach-Spec. Den Status führt allein die Capability-Matrix, die Reihenfolge allein die Roadmap. Wer eine Abweichung findet, korrigiert das nachrangige Dokument im selben Change.

## Lesereihenfolge

1. diese Datei
2. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
3. [docs/ROADMAP.md](docs/ROADMAP.md)
4. die Fach-Spec bzw. das ADR für den jeweiligen Concern
