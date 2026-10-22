# Frontend / Backend Contract

## Purpose

This document defines the clean connection boundary between the standalone frontend surface and the backend HTTP API.

It exists to keep three concerns separate:

- frontend presentation and interaction
- frontend transport/connectors
- backend runtime behavior

## Ownership

Frontend-owned connection code:

- `apps/nrese-console/src/lib/client.ts`
- `apps/nrese-console/src/lib/endpoints.ts`
- `apps/nrese-console/src/lib/http.ts`
- `apps/nrese-console/src/lib/runtimeConfig.ts`
- `apps/nrese-console/src/cli/`

Backend-owned HTTP behavior:

- `crates/nrese-server/src/http/`
- `crates/nrese-server/src/policy.rs`
- `crates/nrese-server/src/state.rs`

Specification source of truth:

- [docs/ops/http-api.md](../ops/http-api.md)

## Rules

- The frontend must talk to the backend only through documented HTTP endpoints.
- Browser UI and CLI must reuse the same frontend-owned TypeScript client boundary.
- Endpoint paths belong in one frontend endpoint registry, not in scattered components or scripts.
- Browser runtime connection settings belong in frontend runtime config, not in component code.
- Backend handlers must not know about frontend routing, CLI flags, or browser-specific behavior.
- If the backend API changes, update the typed frontend client and this contract in the same slice.

## Browser Connection

The browser console supports two connection modes:

- same-origin default
- explicit runtime-configured API base URL

Runtime config is loaded from:

- `apps/nrese-console/public/console-config.js`

The expected shape is:

```js
window.__NRESE_CONSOLE_CONFIG__ = {
  apiBaseUrl: "https://nrese.example.com",
};
```

Build-time override is also supported through:

- `VITE_NRESE_API_BASE_URL`

For local dev proxying:

- `VITE_API_PROXY_TARGET`

For frontend hosting under a different path prefix:

- `VITE_CONSOLE_BASE_PATH`

## CLI Connection

The frontend package also ships a fast CLI on top of the same TypeScript client boundary:

```powershell
Set-Location .\apps\nrese-console
npm run cli -- runtime
npm run cli -- capabilities
npm run cli -- query --text "SELECT * WHERE { ?s ?p ?o } LIMIT 5"
```

Connection inputs:

- `--base-url <url>` or `NRESE_API_BASE_URL`
- `--token <token>` or `NRESE_API_TOKEN`
- repeated `--header name:value`

Supported commands:

- `runtime`
- `capabilities`
- `reasoning`
- `query`
- `update`
- `tell`
- `graph-read`
- `graph-write`
- `graph-delete`

## Clean Extension Pattern

When adding a new backend-facing feature:

1. add or update endpoint constants in `src/lib/endpoints.ts`
2. add or update the typed client method in `src/lib/client.ts`
3. expose it to browser code through `src/lib/api.ts` only if the UI needs a convenience wrapper
4. expose it to CLI only if the command surface benefits from it
5. update tests and docs in the same slice

Do not:

- call `fetch` directly from React components
- duplicate endpoint strings in browser and CLI code
- add one-off node scripts that bypass the shared client
- encode deployment-specific URLs into components

## Acceptance Criteria

- Browser UI can be built and hosted separately from the backend.
- Browser UI can target a non-same-origin backend through runtime config. The backend has no CORS policy of its
  own: the proxy in front of it answers the preflight and sends `Access-Control-Allow-Origin`. The backend marks
  `NRESE-Completeness` with `Access-Control-Expose-Headers` on every query answer that carries it, so the console
  can distinguish complete answers from sound-only, unsound and not-certified on another origin too; a proxy
  that sets its own exposed headers must keep that explicit name (wildcard-only exposure is insufficient
  with credentials).
- `not-certified` and nonempty unrecognized completeness tokens warn that answers may be missing or
  incorrect; neither guarantee is asserted. The CLI preserves stdout and exits 3 with `--require-complete`,
  while HTTP failure remains exit 1. Older N16 maps the new token conservatively to `unsound` and retains
  the raw header/warning; strict third-party enum consumers require an update. Pre-N16 clients that ignore
  completeness do not provide this warning guarantee.
- CLI can talk to the same backend without using browser code paths.
- Endpoint paths and request construction remain centralized in one frontend client boundary.
