export type FetchLike = typeof fetch;

/** An error as the server reports every one (RFC 9457 problem details). */
export type Problem = {
  type: string;
  title: string;
  status: number;
  detail: string;
  /** The request's id, as in the server's log: quote it when reporting a fault. */
  request_id?: string;
};

type ResponseEnvelope = {
  ok: boolean;
  status: number;
  body: string;
  /** The problem document, when the server answered with one. */
  problem?: Problem;
};

/** A failed request, carrying the server's problem document when there was one. */
export class ProblemError extends Error {
  readonly status: number;
  readonly problem?: Problem;

  constructor(status: number, problem?: Problem) {
    super(problem ? describeProblem(problem) : `Request failed with status ${status}`);
    this.name = "ProblemError";
    this.status = status;
    this.problem = problem;
  }
}

/** One line for a problem: title, detail and request id, as the console and CLI show it. */
export function describeProblem(problem: Problem): string {
  const detail =
    problem.detail && problem.detail !== problem.title
      ? `${problem.title}: ${problem.detail}`
      : problem.title;
  return problem.request_id ? `${detail} (request ${problem.request_id})` : detail;
}

function joinUrl(baseUrl: string, path: string): string {
  if (/^https?:\/\//.test(path)) {
    return path;
  }
  if (!baseUrl) {
    return path;
  }
  return `${baseUrl}${path.startsWith("/") ? path : `/${path}`}`;
}

function readProblem(response: Response, body: string): Problem | undefined {
  const media = response.headers?.get?.("content-type") ?? "";
  if (response.ok || !media.startsWith("application/problem+json")) {
    return undefined;
  }
  try {
    const document = JSON.parse(body) as Partial<Problem>;
    if (typeof document.title !== "string" || typeof document.status !== "number") {
      return undefined;
    }
    return {
      type: document.type ?? "about:blank",
      title: document.title,
      status: document.status,
      detail: document.detail ?? "",
      request_id: document.request_id,
    };
  } catch {
    return undefined;
  }
}

export async function fetchText(
  fetchImpl: FetchLike,
  baseUrl: string,
  path: string,
  init?: RequestInit,
): Promise<ResponseEnvelope> {
  const response = await fetchImpl(joinUrl(baseUrl, path), init);
  const body = await response.text();
  const problem = readProblem(response, body);
  return {
    ok: response.ok,
    status: response.status,
    body,
    ...(problem ? { problem } : {}),
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
    throw new ProblemError(response.status, readProblem(response, await response.text()));
  }
  return (await response.json()) as T;
}
