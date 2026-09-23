// Mirrors the serde output of crates/tessera-core/src/model.rs.

export interface Contexts {
  current: string | null;
  contexts: { name: string; cluster: string; user: string | null; namespace: string | null }[];
}

export interface ClusterGraph {
  context: string;
  serverVersion: string | null;
  fetchedAt: string;
  nodes: NodeInfo[];
  ingresses: IngressInfo[];
  services: ServiceInfo[];
  workloads: WorkloadInfo[];
  pods: PodInfo[];
  events: EventInfo[];
  namespaces: { name: string; labels: Record<string, string> }[];
  networkPolicies: unknown[];
  pvcs: { namespace: string; name: string; phase: string; storageClass: string | null }[];
  hpas: unknown[];
  ingressClasses: string[];
  defaultIngressClass: string | null;
  secretNames: string[] | null;
  mesh: { installed: boolean; virtualServices: unknown[]; destinationRules: unknown[] };
  issues: Issue[];
  warnings: string[];
}

export interface NodeInfo {
  name: string;
  ready: boolean;
  unschedulable: boolean;
  instanceType: string | null;
  cpuAllocatableMilli: number;
  memoryAllocatableBytes: number;
  pressure: string[];
}

export interface Route { host: string | null; path: string; service: string; port: string | null }

export interface IngressInfo {
  namespace: string;
  name: string;
  className: string | null;
  addresses: string[];
  routes: Route[];
}

export interface ServiceInfo {
  namespace: string;
  name: string;
  type: string;
  clusterIp: string | null;
  selector: Record<string, string>;
  ports: string[];
  external: string[];
  readyEndpoints: number;
  notReadyEndpoints: number;
  pods: string[];
  workloads: string[];
}

export interface ContainerSpecInfo {
  name: string;
  image: string;
  cpuRequestMilli: number | null;
  memoryRequestBytes: number | null;
  memoryLimitBytes: number | null;
}

export interface WorkloadInfo {
  id: string;
  kind: string;
  namespace: string;
  name: string;
  desired: number;
  ready: number;
  available: number;
  podLabels: Record<string, string>;
  containers: ContainerSpecInfo[];
}

export interface ContainerStatusInfo {
  name: string;
  image: string;
  ready: boolean;
  restartCount: number;
  state: string;
  reason: string | null;
  message: string | null;
  lastReason: string | null;
  lastExitCode: number | null;
  cpuRequestMilli: number | null;
  memoryLimitBytes: number | null;
}

export interface PodInfo {
  namespace: string;
  name: string;
  status: string;
  phase: string;
  ready: boolean;
  restarts: number;
  node: string | null;
  workload: string | null;
  labels: Record<string, string>;
  podIp: string | null;
  created: string | null;
  containers: ContainerStatusInfo[];
}

export interface EventInfo {
  namespace: string;
  kind: string;
  name: string;
  type: string;
  reason: string;
  message: string;
  count: number;
  lastSeen: string | null;
}

export type Severity = "critical" | "warning";
export type Layer = "entry" | "service" | "workload" | "pod" | "node";
export type Category =
  | "routing" | "network" | "dns" | "mesh" | "image" | "config" | "storage"
  | "scheduling" | "capacity" | "scaling" | "runtime" | "node" | "other";

export interface Issue {
  id: string;
  severity: Severity;
  layer: Layer;
  category: Category;
  target: { kind: string; namespace: string; name: string };
  title: string;
  detail: string;
  evidence: string[];
  suggestion: string;
  commands: string[];
  pods: string[];
}
