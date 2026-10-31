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

describe("completeness", () => {
  const soundOnly = {
    ok: true,
    status: 200,
    body: "false",
    completeness: { status: "sound-only" as const, header: "sound-only; regime=owl2-dl" },
  };

  test("an answer not shown to be complete warns on stderr and keeps stdout", () => {
    expect(textReport(soundOnly)).toEqual({
      lines: ["status=200", "false"],
      warnings: ["completeness=sound-only; regime=owl2-dl"],
      exitCode: 0,
    });
    const uncertified = { ...soundOnly, completeness: { status: "not-certified" as const, header: "not-certified" } };
    expect(textReport(uncertified)).toEqual({
      lines: ["status=200", "false"],
      warnings: ["completeness=not-certified; soundness and completeness are not certified; answers may be missing or incorrect"],
      exitCode: 0,
    });
    expect(textReport(uncertified, true).exitCode).toBe(3);
    expect(textReport({ ...uncertified, ok: false, status: 400 }, true)).toEqual({
      lines: ["status=400", "false"], exitCode: 1,
    });
  });

  test("--require-complete fails on it, with its own exit status", () => {
    expect(textReport(soundOnly, true).exitCode).toBe(3);
    const complete = { ...soundOnly, completeness: { status: "complete" as const, header: "complete" } };
    expect(textReport(complete, true)).toEqual({ lines: ["status=200", "false"], exitCode: 0 });
  });
});
