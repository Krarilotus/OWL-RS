import { useQuery } from "@tanstack/react-query";

import { getAiStatus, getCapabilities, getRuntimeSnapshot } from "../lib/api";

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

  return {
    runtimeQuery,
    capabilitiesQuery,
    aiStatusQuery,
  };
}
