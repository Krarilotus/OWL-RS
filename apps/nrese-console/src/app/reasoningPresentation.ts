import type { ReasoningCapability, ReasoningDiagnostics } from "../lib/types";

export function buildReasoningConfigSnippet(
  reasoning?: ReasoningDiagnostics,
): string {
  return `[reasoner]\nmode = "${reasoning?.mode ?? "disabled"}"`;
}

export function sortReasoningCapabilities(
  capabilities: ReasoningCapability[] | undefined,
): ReasoningCapability[] {
  return [...(capabilities ?? [])].sort((left, right) =>
    left.feature.localeCompare(right.feature),
  );
}
