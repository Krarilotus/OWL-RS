import { describe, expect, test } from "vitest";

import { textReport } from "./report";

describe("textReport", () => {
  test("a successful status succeeds", () => {
    for (const status of [200, 201, 204]) {
      expect(textReport({ ok: true, status, body: "" })).toEqual({
        lines: [`status=${status}`],
        exitCode: 0,
      });
    }
  });

  test("a failing status fails the command and still prints the body", () => {
    for (const status of [400, 401, 403, 500]) {
      expect(textReport({ ok: false, status, body: "no" })).toEqual({
        lines: [`status=${status}`, "no"],
        exitCode: 1,
      });
    }
  });

  test("a problem document prints as one line with its request id", () => {
    const problem = {
      type: "https://nrese.dev/problems/not-found",
      title: "Not Found",
      status: 404,
      detail: "no graph <http://e/g>",
      request_id: "req-1",
    };
    expect(textReport({ ok: false, status: 404, body: "{}", problem })).toEqual({
      lines: ["status=404", "error: Not Found: no graph <http://e/g> (request req-1)"],
      exitCode: 1,
    });
  });
});
