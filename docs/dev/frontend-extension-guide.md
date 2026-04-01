# Frontend Extension Guide

## Purpose

This document explains how to extend the NRESE user frontend without mixing styling, language, transport, and feature logic.

## Frontend Ownership

- `apps/nrese-console/src/components/`
  UI components and interaction composition
- `apps/nrese-console/src/app/`
  screen-level orchestration hooks, defaults, and guided workbench examples
- `apps/nrese-console/src/lib/`
  HTTP transport, endpoint registry, shared TypeScript client, browser API wrappers, and shared frontend types
- `apps/nrese-console/src/cli/`
  CLI argument parsing and fast command entry points on top of the same TypeScript client
- `apps/nrese-console/src/i18n/`
  language strings and string-shape types
- `apps/nrese-console/src/styles/`
  design tokens and layout/base styling

## Rules

- Add new backend calls in `src/lib/api.ts` or a focused sibling module.
- Keep endpoint paths in `src/lib/endpoints.ts`.
- Keep the shared TypeScript client in `src/lib/client.ts`.
- Keep screen-level orchestration in `src/app/`, not in `App.tsx`.
- Keep reusable editor templates and guided examples in `src/app/` as explicit data, not as inline literals inside components.
- Keep runtime configuration and reasoning policy visibility driven by server diagnostics, not by frontend-local shadow state.
- Keep raw `fetch` and transport details in `src/lib/http.ts`.
- Keep browser runtime API base selection in `src/lib/runtimeConfig.ts`.
- Keep CLI access on the same client boundary instead of adding separate ad hoc scripts.
- Keep language strings out of components.
- Keep the i18n type shape in `src/i18n/types.ts`.
- Keep color, spacing, and typography values in `src/styles/tokens.css`.
- Keep structural styles in the style files, not in component-local inline style objects.
- If one feature grows beyond a single component and helper file, create a new topic folder under `src/components/` or `src/lib/`.

## Common Customization Points

### Change Text Or Add Languages

- edit `apps/nrese-console/src/i18n/en.ts`
- edit `apps/nrese-console/src/i18n/de.ts`
- add another language file and register it in `apps/nrese-console/src/i18n/index.ts`
- locale selection and persistence are owned in `apps/nrese-console/src/app/useLocale.ts`

### Change Styling

- edit tokens in `apps/nrese-console/src/styles/tokens.css`
- keep structural styles in `base.css` and `layout.css`

### Change Guided Workbench Examples

- edit `apps/nrese-console/src/app/workbenchExamples.ts`
- keep example labels in `apps/nrese-console/src/i18n/*.ts`
- keep example application logic in `App.tsx` or an app-level hook, not inside the presentation component

### Add A New API-backed Feature

1. define request/response types in `src/lib/types.ts` if needed
2. add or update the endpoint constant in `src/lib/endpoints.ts`
3. add or update the shared client method in `src/lib/client.ts`
4. expose a browser convenience wrapper in `src/lib/api.ts` only if the React app needs it
5. add or extend a component in `src/components/`
6. prefer capability- or diagnostics-driven endpoints over hardcoded path forks when the backend already exposes them
7. add or update a frontend or CLI test
8. update README and spec/docs if the user-visible surface changed

### Host The Frontend Separately From The Backend

- use `public/console-config.js` for runtime-configured API base URLs
- use `VITE_NRESE_API_BASE_URL` for build-time API binding when runtime config is not preferred
- use `VITE_CONSOLE_BASE_PATH` when the frontend is served under a different path prefix
- keep the backend as a pure HTTP dependency; do not bake backend assumptions into components

### Use The CLI

The frontend package ships a small CLI on the same client boundary:

```powershell
Set-Location .\apps\nrese-console
npm install
npm run cli -- runtime
npm run cli -- query --text "SELECT * WHERE { ?s ?p ?o } LIMIT 5"
```

For named graph operations:

```powershell
npm run cli -- graph-read --graph named --graph-iri https://example.org/g
```

For auth:

- `--token <token>` or `NRESE_API_TOKEN`
- repeated `--header name:value`

The CLI contract and browser/runtime connection boundary are documented in [frontend-backend-contract.md](./frontend-backend-contract.md).

### Replace Or Extend The AI Provider Path

- keep provider integration on the server side under `crates/nrese-server/src/ai/`
- keep the frontend bound only to `/api/ai/status` and `/api/ai/query-suggestions`
- do not move provider secrets into the frontend

## Build And Test

```powershell
Set-Location .\apps\nrese-console
npm install
npm run build
npm run test -- --run
Set-Location ..\..
```
