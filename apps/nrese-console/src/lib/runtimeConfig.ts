export type ConsoleRuntimeConfig = {
  apiBaseUrl?: string;
};

declare global {
  interface Window {
    __NRESE_CONSOLE_CONFIG__?: ConsoleRuntimeConfig;
  }
}

function trimTrailingSlash(value: string): string {
  return value.replace(/\/+$/, "");
}

export function normalizeApiBaseUrl(value?: string | null): string {
  if (!value) {
    return "";
  }
  return trimTrailingSlash(value);
}

export function readConsoleRuntimeConfig(): ConsoleRuntimeConfig {
  if (typeof window === "undefined") {
    return {};
  }
  return window.__NRESE_CONSOLE_CONFIG__ ?? {};
}

/** The API base URL: the runtime configuration's if it sets one (an empty one means the
 * console's own origin), else the build's, else the console's own origin. The stock
 * `console-config.js` sets none, so a build-time URL holds unless a deployment overrides
 * it (the review of 3 October 2026, C5). */
export function resolveApiBaseUrl(
  runtimeConfig: ConsoleRuntimeConfig,
  buildTimeUrl?: string,
): string {
  return normalizeApiBaseUrl(runtimeConfig.apiBaseUrl ?? buildTimeUrl ?? "");
}

export function resolveBrowserApiBaseUrl(): string {
  return resolveApiBaseUrl(
    readConsoleRuntimeConfig(),
    import.meta.env.VITE_NRESE_API_BASE_URL,
  );
}
