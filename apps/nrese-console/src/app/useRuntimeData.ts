import { useQuery } from "@tanstack/react-query";

import {
  getAiStatus,
  getCapabilities,
  getReasoningDiagnostics,
  getRuntimeSnapshot,
} from "../lib/api";

export function useRuntimeData() {
  const runtimeQuery = useQuery({
    queryKey: ["runtime-snapshot"],
    queryFn: getRuntimeSnapshot,
    refetchInterval: 15000,
  });
  const capabilitiesQuery = useQuery({
    queryKey: ["capabilities"],
    queryFn: getCapabilities,
    staleTime: 30000,
  });
  const aiStatusQuery = useQuery({
    queryKey: ["ai-status"],
    queryFn: getAiStatus,
    retry: false,
  });
  const reasoningQuery = useQuery({
    queryKey: ["reasoning-diagnostics", capabilitiesQuery.data?.reasoning_diagnostics_endpoint],
    enabled: Boolean(capabilitiesQuery.data?.reasoning_diagnostics_endpoint),
    queryFn: () =>
      getReasoningDiagnostics(
        capabilitiesQuery.data?.reasoning_diagnostics_endpoint,
      ),
    staleTime: 30000,
  });

  return {
    runtimeQuery,
    capabilitiesQuery,
    aiStatusQuery,
    reasoningQuery,
  };
}
