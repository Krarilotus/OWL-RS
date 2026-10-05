import { describe, expect, test } from "vitest";

import { ProblemError, fetchJson, fetchText } from "./http";

const problem = {
  type: "https://nrese.dev/problems/bad-request",
  title: "Bad Request",
  status: 400,
  detail: "the query doesn't parse",
  request_id: "req-7",
};

function answer(body: string, status: number, media: string): typeof fetch {
  return async () => new Response(body, { status, headers: { "Content-Type": media } });
}

describe("problem documents", () => {
  test("a failed JSON request throws the server's problem", async () => {
    const fetchImpl = answer(JSON.stringify(problem), 400, "application/problem+json");
    const error = await fetchJson(fetchImpl, "", "/api/v1/x").catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ProblemError);
    expect((error as ProblemError).problem).toEqual(problem);
    expect((error as ProblemError).message).toBe(
      "Bad Request: the query doesn't parse (request req-7)",
    );
  });

  test("a text response keeps its body and reads the problem", async () => {
    const body = JSON.stringify(problem);
    const response = await fetchText(answer(body, 400, "application/problem+json"), "", "/q");
    expect(response).toEqual({ ok: false, status: 400, body, problem });
  });

  test("a plain error has no problem", async () => {
    const response = await fetchText(answer("parse error", 400, "text/plain"), "", "/q");
    expect(response).toEqual({ ok: false, status: 400, body: "parse error" });
    const error = await fetchJson(answer("no", 502, "text/plain"), "", "/x").catch(
      (e: unknown) => e,
    );
    expect((error as ProblemError).message).toBe("Request failed with status 502");
  });
});
