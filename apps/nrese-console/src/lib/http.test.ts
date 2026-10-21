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

describe("completeness", () => {
  const answered = (header?: string): typeof fetch => async () =>
    new Response("{}", {
      status: 200,
      headers: header ? { "NRESE-Completeness": header } : {},
    });

  test("a query's answers carry the status the server states", async () => {
    for (const [header, status] of [
      ["complete; regime=owl2-ql", "complete"],
      ['sound-only; regime=owl2-dl; reasons="dl: open"', "sound-only"],
      ["unsound; regime=owl2-dl", "unsound"],
      // A status this client doesn't know is never taken for complete.
      ["provisional", "unsound"],
    ] as const) {
      const response = await fetchText(answered(header), "", "/q");
      expect(response.completeness).toEqual({ status, header });
    }
    expect((await fetchText(answered(), "", "/q")).completeness).toBeUndefined();
  });
});
