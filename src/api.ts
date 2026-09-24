// Bridge to the Rust side. When the page runs in a plain browser (npm run dev
// without Tauri), it serves a built-in demo cluster instead, which is handy
// for UI work and screenshots.

import type { AuditEntry, ClusterGraph, Contexts, Environment, NetworkTestReport, PlanResponse, Policy, Settings, TestPlan } from "./types";
import { demoAudit, demoContexts, demoGraph, demoLogs, demoPlan, demoPolicy, demoReport } from "./demo";

export const demoMode = !("__TAURI_INTERNALS__" in window);

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export function getPolicy(): Promise<Policy> {
  return demoMode ? Promise.resolve(demoPolicy) : call("get_policy");
}

export function auditLog(limit = 100): Promise<[AuditEntry[], string]> {
  return demoMode ? Promise.resolve([demoAudit.slice(0, limit), "(demo: kept in memory)"]) : call("audit_log", { limit });
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

export function planNetworkTest(
  context: string, namespace: string, service: string, sourceNamespace: string, s: Settings, environmentOverride: Environment | null,
): Promise<PlanResponse> {
  if (demoMode) {
    const env = demoContexts.contexts.find((c) => c.name === context)?.environment ?? "other";
    const effective = environmentOverride === "production" ? "production" : env;
    if (effective === "production" && !demoPolicy.allowNetworkTestsInProduction) {
      demoAudit.unshift({ time: Math.floor(Date.now() / 1000), user: "demo", context, environment: effective, action: "network_test_blocked", namespace, target: `service/${service}`, podsCreated: [], podsDeleted: [], outcome: "Network tests are turned off for production contexts." });
      return Promise.reject("Network tests are turned off for production contexts. An administrator can allow them with allowNetworkTestsInProduction in the policy file.");
    }
    return new Promise((r) => setTimeout(() => r({ id: "demo", plan: demoPlan(namespace, service, sourceNamespace || namespace, s.probeImage), environment: effective, requiresConfirmation: effective === "production" }), 300));
  }
  return call("plan_network_test", {
    context,
    request: { namespace, service, sourceNamespace: sourceNamespace || null, image: s.probeImage || null, clusterDomain: s.clusterDomain || null },
    environmentOverride,
  });
}

export function runNetworkTest(planId: string, plan: TestPlan, context: string, confirmation: string | null): Promise<NetworkTestReport> {
  if (demoMode) {
    return new Promise((r) => setTimeout(() => {
      const rep = demoReport(plan);
      demoAudit.unshift({ time: Math.floor(Date.now() / 1000), user: "demo", context, environment: "other", action: "network_test_run", namespace: plan.service.namespace, target: `service/${plan.service.name}`, podsCreated: rep.podsCreated, podsDeleted: rep.podsDeleted, outcome: `${rep.findings.filter((f) => f.status !== "ok").length} findings` });
      r(rep);
    }, 1500));
  }
  return call("run_network_test", { planId, confirmation });
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
