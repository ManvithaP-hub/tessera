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

Each rule belongs to one layer of the request path.

| Layer | What it catches |
|---|---|
| Entry | Ingress backends that point at services that don't exist |
| Service | Selectors that match no pods, with the likely correct labels; services with no ready endpoints; services whose only workload is scaled to zero |
| Workload | Replicas not ready without a pod-level explanation (often a rollout) |
| Pod | OOMKilled crash loops (with the limit and a suggested value), other crash loops (with exit-code hints), image pull failures (missing tag vs. auth), missing ConfigMaps or Secrets, failing readiness |
| Node | Unschedulable pods (CPU or memory too large for any node, taints, affinity, unbound PVCs), NotReady nodes, pressure conditions, cordoned nodes |

Pod findings are grouped per workload, and service symptoms are downgraded to
warnings when a pod issue already explains them, so the list points at root
causes rather than every symptom.

## Adding a rule

1. Add the check in `crates/tessera-core/src/diagnose.rs` in the function for
   its layer.
2. Give it evidence the user can verify and at least one read-only `kubectl`
   command in `commands`.
3. Add a unit test next to the existing ones, and if it depends on how raw
   objects are converted, extend `tests/snapshot.rs`.
