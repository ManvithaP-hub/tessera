// Bridge to the Rust side. When the page runs in a plain browser (npm run dev
// without Tauri), it serves a built-in demo cluster instead, which is handy
// for UI work and screenshots.

import type { ClusterGraph, Contexts } from "./types";
import { demoContexts, demoGraph, demoLogs } from "./demo";

export const demoMode = !("__TAURI_INTERNALS__" in window);

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export function listContexts(): Promise<Contexts> {
  return demoMode ? Promise.resolve(demoContexts) : call("list_contexts");
}

export function snapshot(context: string): Promise<ClusterGraph> {
  if (demoMode) return new Promise((r) => setTimeout(() => r(demoGraph(context)), 250));
  return call("cluster_snapshot", { context });
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
