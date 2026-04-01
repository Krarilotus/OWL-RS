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

export function resolveBrowserApiBaseUrl(): string {
  const runtimeConfig = readConsoleRuntimeConfig();
  return normalizeApiBaseUrl(
    runtimeConfig.apiBaseUrl ?? import.meta.env.VITE_NRESE_API_BASE_URL ?? "",
  );
}
