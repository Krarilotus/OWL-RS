import { apiPath } from "./basePath";

export async function fetchText(
  path: string,
  init?: RequestInit,
): Promise<{ ok: boolean; status: number; body: string }> {
  const response = await fetch(apiPath(path), init);
  return {
    ok: response.ok,
    status: response.status,
    body: await response.text(),
  };
}

export async function fetchJson<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(apiPath(path), init);
  if (!response.ok) {
    throw new Error(`Request failed with status ${response.status}`);
  }
  return (await response.json()) as T;
}
