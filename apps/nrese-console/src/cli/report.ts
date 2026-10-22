import { type Completeness, describeProblem, type Problem } from "../lib/http";

/** The exit status of answers not shown to be complete, under `--require-complete`. */
export const INCOMPLETE_EXIT_CODE = 3;

/** A text response as the CLI prints it, and the exit status it means: a failing HTTP
 * status fails the command, so shell scripts see it (the review of 3 October 2026, C4).
 * A problem document prints as one line, with the request id to quote. Answers the server
 * doesn't vouch for as complete (`NRESE-Completeness`) add a warning for stderr, leaving
 * stdout as it was; with `requireComplete` they fail the command with
 * [`INCOMPLETE_EXIT_CODE`]. */
export function textReport(
  response: {
    ok: boolean;
    status: number;
    body: string;
    problem?: Problem;
    completeness?: Completeness;
  },
  requireComplete = false,
): {
  lines: string[];
  warnings?: string[];
  exitCode: number;
} {
  const lines = [`status=${response.status}`];
  if (response.problem) {
    lines.push(`error: ${describeProblem(response.problem)}`);
  } else if (response.body) {
    lines.push(response.body);
  }
  const completeness = response.completeness;
  if (response.ok && completeness && completeness.status !== "complete") {
    return {
      lines,
      warnings: [completeness.status === "not-certified"
        ? `completeness=${completeness.header}; soundness and completeness are not certified; answers may be missing or incorrect`
        : `completeness=${completeness.header}`],
      exitCode: requireComplete ? INCOMPLETE_EXIT_CODE : 0,
    };
  }
  return { lines, exitCode: response.ok ? 0 : 1 };
}
