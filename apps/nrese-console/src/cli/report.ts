import { describeProblem, type Problem } from "../lib/http";

/** A text response as the CLI prints it, and the exit status it means: a failing HTTP
 * status fails the command, so shell scripts see it (the review of 3 October 2026, C4).
 * A problem document prints as one line, with the request id to quote. */
export function textReport(response: {
  ok: boolean;
  status: number;
  body: string;
  problem?: Problem;
}): {
  lines: string[];
  exitCode: number;
} {
  const lines = [`status=${response.status}`];
  if (response.problem) {
    lines.push(`error: ${describeProblem(response.problem)}`);
  } else if (response.body) {
    lines.push(response.body);
  }
  return { lines, exitCode: response.ok ? 0 : 1 };
}
