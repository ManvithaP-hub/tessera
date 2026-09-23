// A small, deterministic demo cluster used when running outside Tauri.
// Its issues mirror what tessera-core produces for the same state.

import type { ClusterGraph, Contexts, Issue, PodInfo, ServiceInfo, WorkloadInfo } from "./types";

export const demoContexts: Contexts = {
  current: "demo-shop",
  contexts: [{ name: "demo-shop", cluster: "demo", user: null, namespace: null }],
};

const MI = 1024 * 1024;
const nodes = ["ip-10-0-12-41", "ip-10-0-24-7", "ip-10-0-38-110"];

function wl(ns: string, name: string, desired: number, ready: number, cpu: number, mem: number): WorkloadInfo {
  return {
    id: `Deployment/${ns}/${name}`, kind: "Deployment", namespace: ns, name, desired, ready, available: ready,
    podLabels: { app: name },
    containers: [{ name, image: `ghcr.io/acme/${name}:2.4.1`, cpuRequestMilli: cpu, memoryRequestBytes: mem * MI, memoryLimitBytes: mem * MI }],
  };
}

function pods(w: WorkloadInfo, kind: "ok" | "oom" | "pending" | "pull", n = w.desired): PodInfo[] {
  return Array.from({ length: n }, (_, i) => {
    const bad = kind === "oom" ? i < 2 : kind === "pull" ? i === 0 : kind === "pending";
    const reason = !bad ? null : kind === "oom" ? "CrashLoopBackOff" : kind === "pull" ? "ImagePullBackOff" : null;
    const c = w.containers[0];
    return {
      namespace: w.namespace,
      name: `${w.name}-7c9d${i}f5b8-${["x2kqp", "m8zrt", "b4vwn", "q7hsd"][i % 4]}`,
      status: bad ? (reason ?? "Pending") : "Running",
      phase: kind === "pending" ? "Pending" : "Running",
      ready: !bad,
      restarts: kind === "oom" && bad ? [14, 9][i] : 0,
      node: kind === "pending" ? null : nodes[(i + w.name.length) % 3],
      workload: w.id,
      labels: { app: w.name },
      podIp: bad ? null : `10.0.${i + 1}.${w.name.length * 7}`,
      created: null,
      containers: [{
        name: c.name, image: kind === "pull" && bad ? c.image.replace("2.4.1", "2.5.0-rc1") : c.image,
        ready: !bad, restartCount: kind === "oom" && bad ? [14, 9][i] : 0,
        state: bad ? "waiting" : "running", reason,
        message: kind === "pull" && bad ? `manifest for ghcr.io/acme/${w.name}:2.5.0-rc1 not found` : null,
        lastReason: kind === "oom" && bad ? "OOMKilled" : null, lastExitCode: kind === "oom" && bad ? 137 : null,
        cpuRequestMilli: c.cpuRequestMilli, memoryLimitBytes: c.memoryLimitBytes,
      }],
    };
  });
}

function svc(w: WorkloadInfo, ps: PodInfo[], selector = w.name): ServiceInfo {
  const matched = selector === w.name ? ps : [];
  return {
    namespace: w.namespace, name: w.name, type: "ClusterIP", clusterIp: `10.100.${w.name.length}.${w.name.length * 9}`,
    selector: { app: selector }, ports: ["80→8080/TCP"], external: [],
    readyEndpoints: matched.filter((p) => p.ready).length, notReadyEndpoints: matched.filter((p) => !p.ready).length,
    pods: matched.map((p) => p.name), workloads: selector === w.name ? [w.id] : [],
  };
}

export function demoGraph(context: string): ClusterGraph {
  const store = wl("web", "storefront", 3, 3, 500, 512);
  const checkout = wl("web", "checkout", 2, 1, 250, 384);
  const pay = wl("payments", "payments-api", 3, 1, 500, 256);
  const cat = wl("web", "catalog", 2, 2, 250, 256);
  const rec = wl("ml", "recommender", 2, 0, 6000, 2048);
  const redis = wl("data", "redis-cache", 1, 1, 500, 1024);
  const P = {
    store: pods(store, "ok"), checkout: pods(checkout, "pull"), pay: pods(pay, "oom"),
    cat: pods(cat, "ok"), rec: pods(rec, "pending"), redis: pods(redis, "ok"),
  };
  const allPods = Object.values(P).flat();
  const services = [svc(store, P.store), svc(checkout, P.checkout), svc(pay, P.pay), svc(cat, P.cat, "catalog-svc"), svc(rec, P.rec), svc(redis, P.redis)];
  const issues: Issue[] = [
    {
      id: "entry-missing-backend:web/shop-edge/cart", severity: "critical", layer: "entry",
      target: { kind: "Ingress", namespace: "web", name: "shop-edge" },
      title: "Ingress routes to a service that doesn't exist: cart",
      detail: "shop-edge sends shop.example.com/cart to service cart, but there is no service with that name in web. Requests on these routes fail at the ingress controller.",
      evidence: ["backend service: cart", "services in web: catalog, checkout, storefront"],
      suggestion: "Create service cart in web, or change the ingress backend to one of the existing services.",
      commands: ["kubectl get ingress shop-edge -n web -o yaml", "kubectl get svc -n web"], pods: [],
    },
    {
      id: "svc-no-match:web/catalog", severity: "critical", layer: "service",
      target: { kind: "Service", namespace: "web", name: "catalog" },
      title: "Service catalog selects no pods",
      detail: "No pod in web has the labels app=catalog-svc, so the service has no endpoints and anything routed to it gets connection errors or 503s.",
      evidence: ["selector: app=catalog-svc", "endpoints: <none>", "catalog pods are labelled app=catalog"],
      suggestion: "If this service is meant for catalog, change its selector to app=catalog.",
      commands: ["kubectl describe svc catalog -n web", "kubectl get pods -n web -l app=catalog-svc"], pods: [],
    },
    {
      id: "pod-oom:Deployment/payments/payments-api", severity: "critical", layer: "pod",
      target: { kind: "Deployment", namespace: "payments", name: "payments-api" },
      title: "Deployment payments-api is running out of memory",
      detail: "Container payments-api in 2 pods was killed for using more than its 256Mi memory limit, then restarted.",
      evidence: ["container: payments-api", "last state: Terminated, reason OOMKilled, exit code 137", "restarts across affected pods: 23", "memory limit: 256Mi"],
      suggestion: "Raise the memory limit for payments-api (for example to 512Mi) and set the request to match, or find what grew the process's memory.",
      commands: [`kubectl describe pod ${P.pay[0].name} -n payments`, `kubectl logs ${P.pay[0].name} -n payments -c payments-api --previous`],
      pods: P.pay.slice(0, 2).map((p) => p.name),
    },
    {
      id: "pod-imagepull:Deployment/web/checkout", severity: "critical", layer: "pod",
      target: { kind: "Deployment", namespace: "web", name: "checkout" },
      title: "Deployment checkout can't pull its image",
      detail: "1 pod can't start because the image for container checkout can't be pulled.",
      evidence: ["image: ghcr.io/acme/checkout:2.5.0-rc1", "reason: ImagePullBackOff", "message: manifest for ghcr.io/acme/checkout:2.5.0-rc1 not found"],
      suggestion: "The tag in ghcr.io/acme/checkout:2.5.0-rc1 doesn't exist in the registry. Fix the tag or push the image, then roll out again.",
      commands: [`kubectl describe pod ${P.checkout[0].name} -n web`], pods: [P.checkout[0].name],
    },
    {
      id: "pod-unschedulable:Deployment/ml/recommender", severity: "critical", layer: "node",
      target: { kind: "Deployment", namespace: "ml", name: "recommender" },
      title: "Deployment recommender can't be scheduled",
      detail: "2 pods stuck in Pending because no node can take them.",
      evidence: ["FailedScheduling: 0/3 nodes are available: 3 Insufficient cpu.", "requested cpu: 6, largest node allocatable: 3.92"],
      suggestion: "No node is big enough for a 6 CPU request. Lower the request or add a node group with larger instances.",
      commands: [`kubectl describe pod ${P.rec[0].name} -n ml`], pods: P.rec.map((p) => p.name),
    },
    {
      id: "svc-no-ready:ml/recommender", severity: "warning", layer: "service",
      target: { kind: "Service", namespace: "ml", name: "recommender" },
      title: "Service recommender has no ready endpoints",
      detail: "The selector matches 2 pods, but none are ready. See the pod issues below for the cause.",
      evidence: ["selector: app=recommender", "ready endpoints: 0, not ready: 2"],
      suggestion: "Check the readiness probe and the pods' recent events.",
      commands: ["kubectl describe svc recommender -n ml"], pods: P.rec.map((p) => p.name),
    },
  ];
  return {
    context, serverVersion: "v1.31.2-demo", fetchedAt: String(Math.floor(Date.now() / 1000)),
    nodes: nodes.map((name) => ({ name, ready: true, unschedulable: false, instanceType: "m6i.xlarge", cpuAllocatableMilli: 3920, memoryAllocatableBytes: 14.8 * 1024 * MI, pressure: [] })),
    ingresses: [{
      namespace: "web", name: "shop-edge", className: "alb", addresses: ["k8s-shopedge-7f3a9c.us-east-1.elb.amazonaws.com"],
      routes: [
        { host: "shop.example.com", path: "/", service: "storefront", port: "80" },
        { host: "shop.example.com", path: "/checkout", service: "checkout", port: "80" },
        { host: "shop.example.com", path: "/api/catalog", service: "catalog", port: "80" },
        { host: "shop.example.com", path: "/cart", service: "cart", port: "80" },
      ],
    }, {
      namespace: "payments", name: "payments-edge", className: "alb", addresses: ["k8s-payments-41bd2e.us-east-1.elb.amazonaws.com"],
      routes: [{ host: "pay.example.com", path: "/api/pay", service: "payments-api", port: "80" }],
    }],
    services, workloads: [store, checkout, pay, cat, rec, redis], pods: allPods,
    events: [
      { namespace: "payments", kind: "Pod", name: P.pay[0].name, type: "Warning", reason: "BackOff", message: "Back-off restarting failed container payments-api", count: 212, lastSeen: null },
      { namespace: "ml", kind: "Pod", name: P.rec[0].name, type: "Warning", reason: "FailedScheduling", message: "0/3 nodes are available: 3 Insufficient cpu.", count: 41, lastSeen: null },
      { namespace: "web", kind: "Pod", name: P.checkout[0].name, type: "Warning", reason: "Failed", message: "Failed to pull image \"ghcr.io/acme/checkout:2.5.0-rc1\": not found", count: 18, lastSeen: null },
    ],
    issues,
    warnings: [],
  };
}

export function demoLogs(pod: string, previous: boolean): string {
  const t = (s: number) => new Date(Date.now() - s * 1000).toISOString();
  if (pod.startsWith("payments-api") && previous) {
    return [
      `${t(96)} INFO  Starting payments-api 2.4.1 on port 8080`,
      `${t(93)} INFO  Rate cache warming: 18,000 merchant keys`,
      `${t(40)} WARN  Heap 231Mi of 256Mi container limit`,
      `${t(22)} WARN  GC overhead 41% over the last 10s`,
    ].join("\n");
  }
  if (pod.startsWith("catalog")) return Array.from({ length: 6 }, (_, i) => `${t(60 - i * 10)} INFO  GET /healthz 200 1ms`).join("\n");
  return Array.from({ length: 8 }, (_, i) => `${t(80 - i * 9)} INFO  GET /${i % 2 ? "products/42" : ""} 200 ${8 + i * 3}ms`).join("\n");
}
