// Bridge to the Rust side. When the page runs in a plain browser (npm run dev
// without Tauri), it serves a built-in demo cluster instead, which is handy
// for UI work and screenshots.

import type { ClusterGraph, Contexts, NetworkTestReport, Settings, TestPlan } from "./types";
import { demoContexts, demoGraph, demoLogs, demoPlan, demoReport } from "./demo";

export const demoMode = !("__TAURI_INTERNALS__" in window);

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export function listContexts(): Promise<Contexts> {
  return demoMode ? Promise.resolve(demoContexts) : call("list_contexts");
}

export function snapshot(context: string, s: Settings): Promise<ClusterGraph> {
  if (demoMode) return new Promise((r) => setTimeout(() => r(demoGraph(context, s.cloudChecks)), 250));
  return call("cluster_snapshot", {
    context,
    options: { cloudChecks: s.cloudChecks, awsProfile: s.awsProfile || null, awsRegion: s.awsRegion || null },
  });
}

export function planNetworkTest(context: string, namespace: string, service: string, sourceNamespace: string, s: Settings): Promise<{ id: string; plan: TestPlan }> {
  if (demoMode) return new Promise((r) => setTimeout(() => r({ id: "demo", plan: demoPlan(namespace, service, sourceNamespace || namespace, s.probeImage) }), 300));
  return call("plan_network_test", {
    context,
    request: { namespace, service, sourceNamespace: sourceNamespace || null, image: s.probeImage || null, clusterDomain: s.clusterDomain || null },
  });
}

export function runNetworkTest(planId: string, plan: TestPlan): Promise<NetworkTestReport> {
  if (demoMode) return new Promise((r) => setTimeout(() => r(demoReport(plan)), 1500));
  return call("run_network_test", { planId });
}

export function podLogs(
  context: string,
  namespace: string,
  name: string,
  container: string | null,
  previous: boolean,
  tail = 500,
): Promise<string> {
  if (demoMode) return Promise.resolve(demoLogs(name, previous));
  return call("pod_logs", { context, namespace, name, container, previous, tail });
}

export function resetConnection(context: string): Promise<void> {
  return demoMode ? Promise.resolve() : call("reset_connection", { context });
}
