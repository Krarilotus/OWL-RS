/** A text response as the CLI prints it, and the exit status it means: a failing HTTP
 * status fails the command, so shell scripts see it (the review of 3 October 2026, C4). */
export function textReport(response: { ok: boolean; status: number; body: string }): {
  lines: string[];
  exitCode: number;
} {
  const lines = [`status=${response.status}`];
  if (response.body) {
    lines.push(response.body);
  }
  return { lines, exitCode: response.ok ? 0 : 1 };
}
