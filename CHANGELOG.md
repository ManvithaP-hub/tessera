# Changelog

## 0.2.0: environment guardrails

- **Production protection.** Contexts are classified as production, staging or
  development from their names. Production shows a red banner, and network
  tests are off there by default; when an administrator allows them, the
  context name must be typed to confirm. Enforced in the backend.
- **Per-cluster settings.** AWS profile, region, probe image and cluster domain
  are now saved per context. The previous global values aren't carried over;
  set them again under Settings → This cluster.
- **Organisation policy file.** `~/.tessera/policy.json` and a system-wide file
  can turn network tests or cloud checks off, allow or hide contexts, and change
  environment detection. The system file overrides users and fails safe.
- **Activity log.** Network tests, the probe pods they created and deleted, and
  blocked attempts are recorded locally and shown in Settings.
- **Environment in the Context menu**, and a coloured stripe on the sidebar.
- New guide: `docs/multi-environment.md`, with SSO profile setup, RBAC, IAM
  (`docs/iam-policy.json`), a namespace-scoped network-test role
  (`docs/rbac-network-tests.yaml`) and an example policy.

## 0.1.1

- Refresh now re-reads your kubeconfig, so clusters you add appear without restarting Tessera.
- Clearer connection errors: an unreachable or deleted cluster, expired cloud login, rejected credentials, missing login helper or bad certificate each get a plain explanation, with the original error kept for bug reports.
- Node labels on the traffic map say "1 pod" instead of "1 pods".
- README explains which Mac download to choose (Apple Silicon or Intel).

## 0.1.0

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
