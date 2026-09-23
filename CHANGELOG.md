# Changelog

## 0.1.0 (unreleased)

First public release.

- Traffic map from ingress and load balancer entry points through services,
  workloads and pods to nodes, with broken hops highlighted.
- Diagnosis across five layers: missing ingress backends, selector mismatches,
  empty endpoints, OOMKilled and other crash loops, image pull failures,
  missing config, unschedulable pods, node conditions.
- Each issue shows the request path, evidence, a suggested fix and read-only
  kubectl commands to confirm it.
- Pod logs, including the previous container.
- Works with any kubeconfig, including exec plugins (EKS, GKE, AKS, OIDC).
- Read-only by design. No telemetry.
