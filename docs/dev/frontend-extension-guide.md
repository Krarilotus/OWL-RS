# Frontend Extension Guide

## Purpose

This document explains how to extend the NRESE user frontend without mixing styling, language, transport, and feature logic.

## Frontend Ownership

- `apps/nrese-console/src/components/`
  UI components and interaction composition
- `apps/nrese-console/src/app/`
  screen-level orchestration hooks, defaults, and guided workbench examples
- `apps/nrese-console/src/lib/`
  HTTP transport, API calls, and shared frontend types
- `apps/nrese-console/src/i18n/`
  language strings and string-shape types
- `apps/nrese-console/src/styles/`
  design tokens and layout/base styling

## Rules

- Add new backend calls in `src/lib/api.ts` or a focused sibling module.
- Keep screen-level orchestration in `src/app/`, not in `App.tsx`.
- Keep reusable editor templates and guided examples in `src/app/` as explicit data, not as inline literals inside components.
- Keep runtime configuration and reasoning policy visibility driven by server diagnostics, not by frontend-local shadow state.
- Keep raw `fetch` and transport details in `src/lib/http.ts`.
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
2. add the API call in `src/lib/api.ts`
3. add or extend a component in `src/components/`
4. prefer capability- or diagnostics-driven endpoints over hardcoded path forks when the backend already exposes them
5. add or update a frontend test
6. update README and spec/docs if the user-visible surface changed

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
