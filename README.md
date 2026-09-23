<p align="center"><img src="app-icon.png" width="84" alt=""></p>

<h1 align="center">Tessera</h1>

<p align="center">A read-only Kubernetes desktop app that shows you <em>where</em> a request breaks.</p>

<p align="center">
  <a href="https://github.com/ManvithaP-hub/tessera/actions/workflows/ci.yml"><img src="https://github.com/ManvithaP-hub/tessera/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License: Apache 2.0"></a>
</p>

![Traffic map](docs/screenshot-map.png)

Most Kubernetes GUIs show you lists of resources. When something is down, you
still have to work out which hop failed: the ingress, the service, the pods, or
the node they should be running on. Tessera draws every request path from the
entry point to the node, marks the broken hop, and tells you why it's broken
and what to change.

## What it does

- **Traffic map.** Ingress and load balancer entry points, services, workloads,
  pods and nodes, connected the way traffic actually flows. Broken hops are
  drawn in red.
- **Diagnosis across 12 categories.** Routing, network policies, cluster DNS,
  service mesh (Istio), images, config and admission policies, storage,
  scheduling, quotas, autoscaling, crashes and probes, and nodes. See the
  [full list of checks](docs/architecture.md#diagnosis-rules).
- **Root cause, not symptoms.** Pod findings are grouped per workload, and a
  service with no endpoints is marked as a symptom when its pods explain why.
- **Evidence you can check.** Every issue lists what was observed and the
  read-only `kubectl` commands to confirm it yourself.
- **Cloud load balancer target health (AWS).** Which ALB, NLB or Classic ELB
  targets are unhealthy and why (wrong status code, timeouts, AZ not enabled,
  stale targets), cross-checked with your readiness probes. GKE backend health
  is read straight from the ingress.
- **Network tests you approve.** Probe pods on the target's node and on another
  node test DNS, the service IP, each pod IP and the pods' own health
  endpoints. Comparing them tells apart DNS faults, kube-proxy faults,
  cross-node CNI faults, NetworkPolicy drops, and apps not listening.
- **Pod networking and probe health.** CNI agents and kube-proxy not ready, VPC
  CNI IP exhaustion, nodes with NetworkUnavailable, probe ports that don't
  exist, probes timing out, liveness checks identical to readiness, and slow
  starters killed by liveness.
- **Pod logs,** including the previous container after a crash.
- **Any cluster your kubeconfig reaches.** EKS, GKE, AKS, kind, k3s and others,
  including exec credential plugins such as `aws eks get-token`.

![Issue trace](docs/screenshot-issues.png)

![Network test](docs/screenshot-network-test.png)

## Safe by design

- **Read-only by default.** Tessera only uses `get` and `list`, plus reading
  pod logs. A minimal ClusterRole is in [`docs/rbac.yaml`](docs/rbac.yaml).
- **Active tests only with your approval.** A network test shows you the exact
  probe pods first, and creates nothing until you click Run. The pods are
  non-root, have no service account token or capabilities, stop after 90
  seconds, and are deleted when the test ends.
- **Cloud checks are opt-in.** Load balancer target health uses your existing
  `aws` CLI with a fixed list of read-only `describe` calls. No cloud
  credentials are stored.
- **No telemetry.** It talks to your API servers and nothing else.
- **Local only.** Credentials stay in memory in the Rust process. The UI can't
  touch your filesystem or the network.

## Install

Download the installer for your platform from
[Releases](https://github.com/ManvithaP-hub/tessera/releases).

Early builds are not code-signed yet:

- **macOS:** if macOS says the app is damaged or can't be opened, run
  `xattr -dr com.apple.quarantine /Applications/Tessera.app` once.
- **Windows:** in the SmartScreen prompt, select *More info*, then *Run anyway*.

Tessera reads `$KUBECONFIG` or `~/.kube/config`. If `kubectl get pods` works in
your terminal, Tessera will too.

## Build from source

Prerequisites: Rust (stable), Node.js 22+, and the
[Tauri system dependencies](https://v2.tauri.app/start/prerequisites/) for your OS.

```sh
git clone https://github.com/ManvithaP-hub/tessera
cd tessera
npm install
npm run tauri dev        # run the desktop app
npm run tauri build      # build an installer for this OS
```

To work on the UI without a cluster, run `npm run dev` and open
<http://localhost:1420>. In a plain browser Tessera uses built-in demo data.

## Project layout

```
crates/tessera-core   Rust engine: kubeconfig, snapshot, diagnosis rules, tests
src-tauri             Desktop shell (Tauri 2)
src                   TypeScript UI
docs                  Architecture notes and RBAC example
```

See [docs/architecture.md](docs/architecture.md) for how the pieces fit and how
to add a diagnosis rule.

## Roadmap

- Watch-based live updates instead of polling
- Azure load balancer and Application Gateway health; GKE L4 load balancers
- Testing from a specific workload's point of view (ephemeral containers)
- Gateway API, Linkerd and Traefik routes
- Optional AI explanation of an issue using a model you choose (your own API
  key, Amazon Bedrock in your account, or a local model), off by default
- Signed and notarized builds

## Contributing

Issues and pull requests are welcome. Please read
[CONTRIBUTING.md](CONTRIBUTING.md) first; the short version is that Tessera
stays read-only and every diagnosis needs evidence and a test.

## License

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
