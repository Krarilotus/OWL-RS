export type FetchLike = typeof fetch;

type ResponseEnvelope = {
  ok: boolean;
  status: number;
  body: string;
};

function joinUrl(baseUrl: string, path: string): string {
  if (/^https?:\/\//.test(path)) {
    return path;
  }
  if (!baseUrl) {
    return path;
  }
  return `${baseUrl}${path.startsWith("/") ? path : `/${path}`}`;
}

export async function fetchText(
  fetchImpl: FetchLike,
  baseUrl: string,
  path: string,
  init?: RequestInit,
): Promise<ResponseEnvelope> {
  const response = await fetchImpl(joinUrl(baseUrl, path), init);
  return {
    ok: response.ok,
    status: response.status,
    body: await response.text(),
  };
}

export async function fetchJson<T>(
  fetchImpl: FetchLike,
  baseUrl: string,
  path: string,
  init?: RequestInit,
): Promise<T> {
  const response = await fetchImpl(joinUrl(baseUrl, path), init);
  if (!response.ok) {
    throw new Error(`Request failed with status ${response.status}`);
  }
  return (await response.json()) as T;
}
