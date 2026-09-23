# Architecture

Tessera has three parts.

**`crates/tessera-core`** is a plain Rust library with no UI code. It loads the
kubeconfig with [kube-rs](https://kube.rs), lists ten resource types in
parallel, converts them into a `ClusterGraph`, and runs the diagnosis rules. The
conversion (`build_graph`) and the rules (`diagnose`) are pure functions, so
they are tested with JSON fixtures and never need a live cluster.

**`src-tauri`** is the desktop shell. It exposes four commands to the webview:
`list_contexts`, `cluster_snapshot`, `pod_logs` and `reset_connection`. It caches
one API client per context and drops it after a failure, so re-authenticating
(for example `aws sso login`) and pressing Refresh is enough to recover. On
macOS and Linux it also imports `PATH` from the login shell, because apps
launched from the Dock don't inherit it and exec credential plugins would
otherwise not be found.

**`src/`** is the TypeScript frontend, built with Vite and no framework. It
renders the traffic map as SVG and traces each issue through the layers a
request passes. When opened in a normal browser (`npm run dev`), it switches
to built-in demo data, which makes UI work possible without a cluster.

## Data flow

```
kubeconfig ──► kube::Client ──► list (nodes, pods, services, endpointslices,
                                      ingresses, deployments, statefulsets,
                                      daemonsets, replicasets, warning events)
                     │
                     ▼
              build_graph()  ──►  diagnose()  ──►  ClusterGraph (JSON)  ──►  webview
```

## Diagnosis rules

Each issue has a **layer** (where on the request path it sits) and a
**category** (what kind of problem it is). Categories are derived from the rule
id in `Category::for_rule`, so every rule is classified in one place.

| Category | What Tessera catches |
|---|---|
| Routing | Ingress backends pointing at missing services; ingress classes that don't exist or no default class; ingresses with no address; missing TLS secrets; cloud load balancers that failed to provision (with subnet and quota hints) or never got an address; selectors that match no pods (with the likely correct labels); no ready endpoints; named target ports no pod defines; target ports that don't match declared container ports |
| Network policy | Policies that block inbound traffic to a service's port; policies that only admit specific sources on a routed service (ingress controller may be excluded); egress policies that block DNS on port 53 |
| DNS | Cluster DNS (kube-dns/CoreDNS) with no ready endpoints; degraded CoreDNS replicas; node resolv.conf being truncated (DNSConfigForming) |
| Service mesh | Pods missing the Istio sidecar in injected namespaces; VirtualService destinations that don't exist; subsets no DestinationRule defines; subsets whose labels match no pods |
| Images | Pull failures, split into missing tag vs. registry auth |
| Config and admission | Missing ConfigMaps or Secrets (env or volume); pods rejected by admission webhooks or Pod Security |
| Storage | Unbound PVCs (missing storage class, CSI driver not provisioning); volumes that fail to mount or attach, including Multi-Attach during rollouts |
| Scheduling | Pods too large for any node, taints, affinity, unbound claims |
| Quota | Pods refused because a namespace ResourceQuota is exhausted |
| Autoscaling | HPAs that can't read metrics (metrics-server, missing requests), HPAs pinned at max, HPAs unable to scale |
| Crashes and probes | OOMKilled crash loops, other crash loops with exit-code hints, crash loops caused by failing liveness probes, readiness failures, rollouts that don't converge |
| Nodes | NotReady, pressure conditions, cordoned nodes, evicted pods |

Rules are deliberately conservative. For example, a NetworkPolicy that uses
`matchExpressions` is skipped rather than guessed at, and the TLS rule only runs
when Secret names can be listed.

Pod findings are grouped per workload, and service symptoms are downgraded to
warnings when a pod issue already explains them, so the list points at root
causes rather than every symptom.

### Not covered yet

Some failures can't be seen from the Kubernetes API alone:
cloud load balancer **target health** (needs cloud credentials), connection
failures between specific pods, CNI or kube-proxy faults, and application-level
errors. These are on the roadmap as optional probes.

## Adding a rule

1. Add the check in `crates/tessera-core/src/diagnose.rs` in the function for
   its layer.
2. Give it evidence the user can verify and at least one read-only `kubectl`
   command in `commands`.
3. Add a unit test next to the existing ones, and if it depends on how raw
   objects are converted, extend `tests/snapshot.rs`.
