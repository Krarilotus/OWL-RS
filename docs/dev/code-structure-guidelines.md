# Code Structure Guidelines

This repository is organized to keep runtime behavior explicit, modular, and easy to change without duplicating logic.

## Core Rules

- Keep one source of truth per concern.
- Prefer explicit data flow and typed preparation artifacts over ad hoc helper chains.
- If a feature needs the same mapping, parsing, or projection in more than one place, extract it.
- If a module starts mixing unrelated responsibilities, split it before adding the next feature.
- Keep tests close to the concern they validate, but out of runtime files once those files start to grow.

## When To Create A Topic Folder

Create a subfolder instead of another flat file when a concern has at least two of these properties:

- more than one preparation/runtime/test helper is needed
- multiple files share the same vocabulary and data types
- there is a public facade plus internal helpers
- the concern is expected to grow in another bounded implementation block

Examples in this repository:

- `crates/nrese-reasoner/src/dataset_index/`
- `crates/nrese-reasoner/src/effective_types/`
- `crates/nrese-reasoner/src/property_consistency/`
- `crates/nrese-server/src/auth/`
- `crates/nrese-server/src/http/`
- `crates/nrese-server/src/http/reasoning_view/`
- `crates/nrese-server/src/config/file_config/`
- `apps/nrese-console/src/components/`

## Single Source Of Truth Rules

- HTTP media-type parsing belongs in one HTTP media module, not in handlers.
- Auth backend behavior belongs in `auth/`, while policy decides routing and enforcement.
- Environment-variable names belong in one config constants module, not as repeated raw string literals.
- Store import/export behavior belongs in `nrese-store`; server code should stay transport-only.
- Reject explanation mapping should be built once and reused by HTTP and operator surfaces.
- Reasoner preparation artifacts should be built once per execution path and reused by all dependent stages.
- Frontend language strings should live in one i18n boundary rather than component-local literals.
- Frontend string types should live in one i18n type module rather than one concrete language file.
- Frontend design tokens should live in one style-token boundary rather than repeated inline values.
- Frontend endpoint paths and request construction should live in one client boundary rather than being reimplemented across components, scripts, and CLI wrappers.
- Topic-folder facades should re-export only the items that are part of the external boundary, not every internal type by default.

## Configuration Ownership

- Parse server/runtime env vars in `crates/nrese-server/src/config/`.
- Own typed runtime config in the crate that owns the behavior.
- For reasoning, `nrese-reasoner` owns `ReasonerConfig` and `RulesMvpFeaturePolicy`, while `nrese-server` owns env parsing and startup error messages.
- Keep env-var names and default knob labels centralized so docs and parsers reference the same identifiers.
- Keep store-specific default behavior in store config unless the server intentionally overrides it.
- If docs mention a config knob, the parser and owning runtime module must exist in the same bounded slice.
- Do not document future config layers such as CLI or config files as available until they are implemented.

## Feature Flag Ownership

- Cargo features are build-time capability switches only.
- Runtime env/config knobs are behavior switches only.
- Do not model the same concern twice unless one side is strictly build-time and the other is strictly runtime.
- If a runtime feature policy changes cache behavior, the policy must participate in cache identity.

## Preparation And Caching Rules

- Cache orchestration and cache invalidation policy live in dedicated cache modules.
- Prepared artifacts should be named and typed.
- Schema-stable artifacts and ABox-sensitive artifacts must not share the same invalidation boundary.
- If a runtime stage depends on grouped/indexed data, build one prepared index and pass it through explicitly.

## Tests

- Use integration tests under `crates/*/tests` for crate-level contracts and service behavior.
- Use module-adjacent test files for internal behavior that benefits from private access.
- Follow Cargo’s default split: unit tests close to the code in `src`, integration-style tests in `tests/`.
- Prefer minimal RDF/Turtle fixtures for behavior that should reflect real ontology handling.
- When a runtime refactor changes boundaries but not behavior, add or update regression tests that prove behavior remained stable.
- If inline tests make a runtime file harder to scan, move them into a topic-adjacent test file instead of leaving them embedded indefinitely.
- Frontend component behavior should be tested from the public component/API surface, not by reaching into styling or implementation details.

## Reactor Step

Every bounded implementation block should end with a reactor pass:

- remove dead code and stale helper paths
- collapse duplicated builders or projections
- review large files for topic-folder extraction
- run `cargo fmt`, clippy, and relevant tests before closing the block

## Contributor Workflow

- identify the owning concern first
- check whether the concern already has a topic folder
- if similar logic exists elsewhere, extract a shared source before adding more copies
- keep runtime files focused on runtime behavior, and move growing test blocks into dedicated test files
- keep frontend API clients, components, strings, and styling in separate folders
- keep frontend CLI entry points on top of the same client boundary instead of adding standalone transport code
- update README/spec/ops docs when behavior, config, or bounded scope changes
- finish the slice with formatter, lints, and the relevant test set

## Current Size/Structure Watchlist

- `crates/nrese-reasoner/src/class_consistency/mod.rs`
- `crates/nrese-server/src/http/handlers.rs`
- `crates/nrese-server/src/http/responses.rs`
- `crates/nrese-server/src/config/auth_env.rs`
- `crates/nrese-server/src/http/reasoning_view/mapping.rs`
