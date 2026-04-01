import type { ReasoningCapability, ReasoningDiagnostics } from "../lib/types";

export function buildReasoningConfigSnippet(
  reasoning?: ReasoningDiagnostics,
): string {
  const policy = reasoning?.configured_policy;
  if (!policy) {
    return "[reasoner]\nmode = \"disabled\"";
  }

  const featureLines = policy.feature_modes
    .map(({ feature, mode }) => `${feature} = "${mode}"`)
    .join("\n");

  return [
    "[reasoner]",
    `mode = "${reasoning?.mode ?? "rules-mvp"}"`,
    "",
    "[reasoner.rules_mvp]",
    `preset = "${policy.preset}"`,
    featureLines,
    `unsupported_constructs = "${policy.unsupported_constructs}"`,
  ]
    .filter(Boolean)
    .join("\n");
}

export function sortReasoningCapabilities(
  capabilities: ReasoningCapability[] | undefined,
): ReasoningCapability[] {
  return [...(capabilities ?? [])].sort((left, right) =>
    left.feature.localeCompare(right.feature),
  );
}
