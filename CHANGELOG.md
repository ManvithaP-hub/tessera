# Changelog

## 0.1.0 (unreleased)

First public release.

- Traffic map from ingress and load balancer entry points through services,
  workloads and pods to nodes, with broken hops highlighted.
- Diagnosis across 12 categories: routing (ingress backends, classes, TLS
  secrets, cloud load balancers, selectors, target ports), network policies
  (blocked inbound ports, blocked DNS egress), cluster DNS, Istio (missing
  sidecars, destinations and subsets), images, config and admission policies,
  storage, scheduling, quotas, autoscaling, crashes and probes, and nodes.
- Issues view can be filtered by category.
- Cloud load balancer target health for AWS (opt-in), and GKE backend health.
- Network tests you approve: probe pods on two nodes separate DNS, kube-proxy,
  CNI, NetworkPolicy and app faults, and call the pods' health endpoints.
- Pod networking checks: CNI agents and kube-proxy, NetworkUnavailable, VPC CNI
  IP exhaustion.
- Probe checks: undefined probe ports, timeouts, liveness equal to readiness,
  missing startup probes.
- Settings view.
- Each issue shows the request path, evidence, a suggested fix and read-only
  kubectl commands to confirm it.
- Pod logs, including the previous container.
- Works with any kubeconfig, including exec plugins (EKS, GKE, AKS, OIDC).
- Read-only by design. No telemetry.
