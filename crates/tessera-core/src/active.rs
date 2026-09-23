//! Active network tests.
//!
//! This is the only part of Tessera that creates anything in a cluster. It
//! runs only when the user asks for a test and approves the plan, which
//! includes the exact pod manifests. Each test creates short-lived probe pods
//! (busybox, non-root, no service account token, no capabilities, 90-second
//! deadline), reads their output, and deletes them.
//!
//! One probe runs on the node of a target pod and, when possible, one on a
//! different node. Comparing what each can reach separates:
//! - DNS problems (names don't resolve),
//! - kube-proxy problems (pod IPs work, the service IP doesn't),
//! - CNI problems (same-node works, cross-node doesn't),
//! - NetworkPolicy or security group drops (connections time out),
//! - app problems (connections refused, health endpoints returning errors).

use crate::model::*;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, DeleteParams, LogParams, PostParams};
use kube::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

pub const DEFAULT_IMAGE: &str = "busybox:1.36.1";
pub const PROBE_LABEL: &str = "app.kubernetes.io/managed-by";
const MAX_PODS: usize = 6;
const MAX_HTTP: usize = 4;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NetworkTestRequest {
    pub namespace: String,
    pub service: String,
    /// Namespace to run probes from; defaults to the service's namespace.
    pub source_namespace: Option<String>,
    /// Image override for registries that can't pull from Docker Hub.
    pub image: Option<String>,
    pub cluster_domain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlannedCheck {
    /// dns, tcp or http
    pub kind: String,
    pub label: String,
    pub host: String,
    pub port: Option<i32>,
    pub path: Option<String>,
    pub timeout: i32,
    /// For pod checks: the node the target pod runs on.
    pub target_node: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedProbe {
    pub node: Option<String>,
    /// same-node, other-node or any
    pub placement: String,
    pub manifest: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestPlan {
    pub service: Target,
    pub source_namespace: String,
    pub image: String,
    pub checks: Vec<PlannedCheck>,
    pub probes: Vec<PlannedProbe>,
    /// Whether a NetworkPolicy selects the target pods (for interpreting drops).
    pub policies_apply: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub kind: String,
    pub label: String,
    pub target: String,
    /// ok, fail, timeout, refused, none, or an HTTP status code
    pub result: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeRun {
    pub node: Option<String>,
    pub placement: String,
    pub results: Vec<CheckResult>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// critical, warning or ok
    pub status: String,
    pub title: String,
    pub detail: String,
    pub suggestion: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkTestReport {
    pub plan: TestPlan,
    pub runs: Vec<ProbeRun>,
    pub findings: Vec<Finding>,
}

/* ---------------- Planning ---------------- */

fn safe_host(s: &str) -> bool {
    !s.is_empty() && s.len() < 254 && s.chars().all(|c| c.is_ascii_alphanumeric() || ".-:".contains(c))
}

fn safe_path(s: &str) -> bool {
    s.starts_with('/') && s.len() < 512 && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._~%?=&+-".contains(c))
}

fn resolve_port(port: &str, pod: &PodInfo) -> Option<i32> {
    port.parse().ok().or_else(|| pod.container_ports.iter().find(|c| c.name.as_deref() == Some(port)).map(|c| c.port))
}

pub fn plan(g: &ClusterGraph, req: &NetworkTestRequest) -> Result<TestPlan, String> {
    let svc = g
        .services
        .iter()
        .find(|s| s.namespace == req.namespace && s.name == req.service)
        .ok_or_else(|| format!("Service {}/{} isn't in the current snapshot.", req.namespace, req.service))?;
    let src = req.source_namespace.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| svc.namespace.clone());
    if !safe_host(&src) {
        return Err("Invalid source namespace.".into());
    }
    let image = req.image.clone().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| DEFAULT_IMAGE.into());
    let domain = req.cluster_domain.clone().filter(|s| safe_host(s)).unwrap_or_else(|| "cluster.local".into());
    let pods: Vec<&PodInfo> = g
        .pods
        .iter()
        .filter(|p| p.namespace == svc.namespace && svc.pods.contains(&p.name) && p.pod_ip.is_some())
        .take(MAX_PODS)
        .collect();
    let mut notes = Vec::new();
    let mut checks = vec![
        PlannedCheck {
            kind: "dns".into(),
            label: "cluster DNS".into(),
            host: format!("kubernetes.default.svc.{domain}"),
            port: None,
            path: None,
            timeout: 3,
            target_node: None,
        },
        PlannedCheck {
            kind: "dns".into(),
            label: "service name".into(),
            host: format!("{}.{}.svc.{domain}", svc.name, svc.namespace),
            port: None,
            path: None,
            timeout: 3,
            target_node: None,
        },
    ];
    let cluster_ip = svc.cluster_ip.clone().filter(|ip| ip != "None" && safe_host(ip));
    for sp in &svc.ports_detail {
        if sp.protocol != "TCP" {
            notes.push(format!("Port {}/{} isn't tested; only TCP is supported.", sp.port, sp.protocol));
            continue;
        }
        if let Some(ip) = &cluster_ip {
            checks.push(PlannedCheck {
                kind: "tcp".into(),
                label: format!("service IP port {}", sp.port),
                host: ip.clone(),
                port: Some(sp.port),
                path: None,
                timeout: 3,
                target_node: None,
            });
        }
        for p in &pods {
            let Some(port) = resolve_port(&sp.target, p) else {
                notes.push(format!("Pod {} doesn't define port {}, so it isn't tested.", p.name, sp.target));
                continue;
            };
            checks.push(PlannedCheck {
                kind: "tcp".into(),
                label: format!("pod {} port {port}", p.name),
                host: p.pod_ip.clone().unwrap_or_default(),
                port: Some(port),
                path: None,
                timeout: 3,
                target_node: p.node.clone(),
            });
        }
    }
    if cluster_ip.is_none() {
        notes.push("Headless service: there's no service IP to test, so only DNS and pod IPs are checked.".into());
    }
    // The pods' own HTTP readiness/liveness endpoints, called over the network.
    let mut http = 0;
    for p in &pods {
        let Some(w) = g.workloads.iter().find(|w| Some(&w.id) == p.workload.as_ref()) else { continue };
        for c in &w.containers {
            for pr in c.probes.iter().filter(|pr| pr.handler == "http" && pr.kind != "startup") {
                if http >= MAX_HTTP {
                    break;
                }
                if pr.scheme.as_deref() == Some("HTTPS") {
                    notes.push(format!(
                        "The {} probe on {} uses HTTPS, which the probe image can't test.",
                        pr.kind, c.name
                    ));
                    continue;
                }
                let (Some(port), Some(path)) = (pr.port.as_deref().and_then(|x| resolve_port(x, p)), pr.path.clone())
                else {
                    continue;
                };
                if !safe_path(&path) {
                    notes.push(format!("The {} probe path on {} has characters Tessera won't put in a shell command, so it isn't tested.", pr.kind, c.name));
                    continue;
                }
                let label = format!("{} probe {} on pod {}", pr.kind, path, p.name);
                if checks.iter().any(|x| {
                    x.kind == "http"
                        && x.host == *p.pod_ip.as_ref().unwrap()
                        && x.path.as_ref() == Some(&path)
                        && x.port == Some(port)
                }) {
                    continue;
                }
                checks.push(PlannedCheck {
                    kind: "http".into(),
                    label,
                    host: p.pod_ip.clone().unwrap_or_default(),
                    port: Some(port),
                    path: Some(path),
                    timeout: pr.timeout.clamp(1, 10),
                    target_node: p.node.clone(),
                });
                http += 1;
            }
        }
    }
    checks.retain(|c| safe_host(&c.host));

    // Placement: on a target pod's node, and on another Ready node if there is one.
    let ready_nodes: Vec<&NodeInfo> = g.nodes.iter().filter(|n| n.ready && !n.unschedulable).collect();
    let same = pods.iter().find_map(|p| p.node.clone()).filter(|n| ready_nodes.iter().any(|r| &r.name == n));
    let other = ready_nodes
        .iter()
        .filter(|n| Some(&n.name) != same.as_ref())
        .max_by_key(|n| !pods.iter().any(|p| p.node.as_ref() == Some(&n.name)))
        .map(|n| n.name.clone());
    let mut placements: Vec<(Option<String>, &str)> = Vec::new();
    match (&same, &other) {
        (Some(s), Some(o)) => {
            placements.push((Some(s.clone()), "same-node"));
            placements.push((Some(o.clone()), "other-node"));
        }
        (Some(s), None) => {
            placements.push((Some(s.clone()), "same-node"));
            notes.push("Only one usable node, so cross-node networking can't be compared.".into());
        }
        _ => placements.push((None, "any")),
    }
    let script = script_for(&checks);
    let probes = placements
        .into_iter()
        .map(|(node, placement)| PlannedProbe {
            manifest: manifest(&src, node.as_deref(), &image, &script),
            node,
            placement: placement.into(),
        })
        .collect();
    let policies_apply = g.network_policies.iter().any(|np| {
        np.namespace == svc.namespace
            && np.ingress_type
            && pods.iter().any(|p| np.pod_selector.is_empty() || labels_match(&np.pod_selector, &p.labels))
    });
    notes.push(format!(
        "Probes run as pods in {src} labelled {PROBE_LABEL}=tessera. NetworkPolicies that admit only specific pod labels will treat them differently from your real clients."
    ));
    Ok(TestPlan {
        service: Target { kind: "Service".into(), namespace: svc.namespace.clone(), name: svc.name.clone() },
        source_namespace: src,
        image,
        checks,
        probes,
        policies_apply,
        notes,
    })
}

/// The busybox script. Every value interpolated here has passed `safe_host`
/// or `safe_path` and is single-quoted.
pub fn script_for(checks: &[PlannedCheck]) -> String {
    let mut s = String::from(
        r#"r(){ echo "R|$1|$2|$3|$4|$5"; }
dns(){ o=$(nslookup "$2" 2>&1); if echo "$o" | grep -q '^Name:'; then r dns "$1" "$2" ok "$(echo "$o" | grep -A1 '^Name:' | grep -m1 'Address' | awk '{print $NF}')"; else r dns "$1" "$2" fail "$(echo "$o" | grep -v '^$' | tail -1)"; fi; }
tcp(){ s=$(date +%s); if nc -z -w "$4" "$2" "$3" 2>/dev/null; then r tcp "$1" "$2:$3" ok "$(( $(date +%s)-s ))s"; else e=$(( $(date +%s)-s )); if [ "$e" -ge 2 ]; then r tcp "$1" "$2:$3" timeout "${e}s"; else r tcp "$1" "$2:$3" refused "${e}s"; fi; fi; }
http(){ s=$(date +%s); c=$(wget -q -S -T "$4" -O /dev/null "http://$2:$3$5" 2>&1 | grep -o 'HTTP/[0-9.]* [0-9][0-9][0-9]' | tail -1 | awk '{print $2}'); e=$(( $(date +%s)-s )); r http "$1" "$2:$3$5" "${c:-none}" "${e}s"; }
"#,
    );
    for c in checks {
        let q = |v: &str| format!("'{v}'");
        let line = match c.kind.as_str() {
            "dns" => format!("dns {} {}\n", q(&c.label), q(&c.host)),
            "tcp" => format!("tcp {} {} {} {}\n", q(&c.label), q(&c.host), c.port.unwrap_or(0), c.timeout),
            "http" => format!(
                "http {} {} {} {} {}\n",
                q(&c.label),
                q(&c.host),
                c.port.unwrap_or(0),
                c.timeout,
                q(c.path.as_deref().unwrap_or("/"))
            ),
            _ => continue,
        };
        s.push_str(&line);
    }
    s.push_str("echo DONE\n");
    s
}

pub fn manifest(namespace: &str, node: Option<&str>, image: &str, script: &str) -> serde_json::Value {
    let mut spec = json!({
        "restartPolicy": "Never",
        "activeDeadlineSeconds": 90,
        "terminationGracePeriodSeconds": 0,
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "tolerations": [{"operator": "Exists"}],
        "securityContext": {"runAsNonRoot": true, "runAsUser": 65534, "runAsGroup": 65534, "seccompProfile": {"type": "RuntimeDefault"}},
        "containers": [{
            "name": "probe",
            "image": image,
            "command": ["sh", "-c", script],
            "resources": {"requests": {"cpu": "10m", "memory": "16Mi"}, "limits": {"cpu": "100m", "memory": "32Mi"}},
            "securityContext": {"allowPrivilegeEscalation": false, "readOnlyRootFilesystem": true, "capabilities": {"drop": ["ALL"]}}
        }]
    });
    if let Some(n) = node {
        spec["nodeName"] = json!(n);
    }
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "generateName": "tessera-probe-",
            "namespace": namespace,
            "labels": {PROBE_LABEL: "tessera", "tessera.dev/probe": "network"}
        },
        "spec": spec
    })
}

/* ---------------- Running ---------------- */

pub fn parse_output(out: &str) -> Vec<CheckResult> {
    out.lines()
        .filter_map(|l| l.strip_prefix("R|"))
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(5, '|').collect();
            (f.len() == 5).then(|| CheckResult {
                kind: f[0].into(),
                label: f[1].into(),
                target: f[2].into(),
                result: f[3].into(),
                detail: f[4].trim().into(),
            })
        })
        .collect()
}

pub async fn run(client: Client, plan: TestPlan) -> NetworkTestReport {
    let api: Api<Pod> = Api::namespaced(client, &plan.source_namespace);
    let mut created: Vec<(usize, String)> = Vec::new();
    let mut runs: Vec<ProbeRun> = plan
        .probes
        .iter()
        .map(|p| ProbeRun { node: p.node.clone(), placement: p.placement.clone(), results: vec![], error: None })
        .collect();

    for (i, p) in plan.probes.iter().enumerate() {
        match serde_json::from_value::<Pod>(p.manifest.clone()) {
            Ok(pod) => match api.create(&PostParams::default(), &pod).await {
                Ok(c) => created.push((i, c.metadata.name.unwrap_or_default())),
                Err(e) => {
                    runs[i].error = Some(if e.to_string().contains("403") || e.to_string().contains("forbidden") {
                        format!(
                            "Your credentials can't create pods in {}, so the test couldn't run there. {e}",
                            plan.source_namespace
                        )
                    } else {
                        format!("Couldn't create the probe pod: {e}")
                    })
                }
            },
            Err(e) => runs[i].error = Some(format!("Invalid probe manifest: {e}")),
        }
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(80);
    let mut pending: Vec<(usize, String)> = created.clone();
    while !pending.is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut still = Vec::new();
        for (i, name) in pending {
            let Ok(pod) = api.get(&name).await else {
                still.push((i, name));
                continue;
            };
            let st = pod.status.clone().unwrap_or_default();
            let phase = st.phase.clone().unwrap_or_default();
            let waiting = st.container_statuses.unwrap_or_default().into_iter().find_map(|c| {
                c.state.and_then(|s| s.waiting).and_then(|w| w.reason.map(|r| (r, w.message.unwrap_or_default())))
            });
            if phase == "Succeeded" || phase == "Failed" {
                match api.logs(&name, &LogParams::default()).await {
                    Ok(out) => {
                        runs[i].results = parse_output(&out);
                        if !out.contains("DONE") {
                            runs[i].error = Some("The probe stopped before finishing all checks.".into());
                        }
                    }
                    Err(e) => runs[i].error = Some(format!("Couldn't read probe output: {e}")),
                }
            } else if let Some((r, m)) =
                waiting.filter(|(r, _)| r.contains("ImagePull") || r == "ErrImagePull" || r == "InvalidImageName")
            {
                runs[i].error = Some(format!(
                    "The probe image {} couldn't be pulled ({r}: {m}). If your cluster can't reach Docker Hub, set a mirrored busybox image in Settings.",
                    plan.image
                ));
            } else {
                still.push((i, name));
            }
        }
        pending = still;
    }
    for (i, _) in &pending {
        runs[*i].error.get_or_insert_with(|| {
            "The probe didn't finish within 80 seconds (it may not have been scheduled).".into()
        });
    }
    // Always clean up, whatever happened.
    for (_, name) in &created {
        let _ = api.delete(name, &DeleteParams::background()).await;
    }
    let findings = analyze(&plan, &runs);
    NetworkTestReport { plan, runs, findings }
}

/* ---------------- Analysis ---------------- */

fn f(status: &str, title: String, detail: String, suggestion: &str) -> Finding {
    Finding { status: status.into(), title, detail, suggestion: suggestion.into() }
}

pub fn analyze(plan: &TestPlan, runs: &[ProbeRun]) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    let svc = format!("{}/{}", plan.service.namespace, plan.service.name);
    let get = |r: &ProbeRun, label: &str| r.results.iter().find(|x| x.label == label).cloned();
    let node_of = |label: &str| plan.checks.iter().find(|c| c.label == label).and_then(|c| c.target_node.clone());
    let ok_runs: Vec<&ProbeRun> = runs.iter().filter(|r| !r.results.is_empty()).collect();
    for r in runs.iter().filter(|r| r.error.is_some()) {
        out.push(f(
            "warning",
            format!("The {} probe didn't complete", r.placement.replace('-', " ")),
            r.error.clone().unwrap_or_default(),
            "Fix the cause above and run the test again.",
        ));
    }
    if ok_runs.is_empty() {
        return out;
    }

    // DNS
    let dns_fail: Vec<&&ProbeRun> =
        ok_runs.iter().filter(|r| get(r, "cluster DNS").is_some_and(|x| x.result != "ok")).collect();
    if !dns_fail.is_empty() {
        out.push(f(
            "critical",
            format!("Pods in {} can't resolve names with cluster DNS", plan.source_namespace),
            format!("Looking up kubernetes.default failed: {}", get(dns_fail[0], "cluster DNS").map(|x| x.detail).unwrap_or_default()),
            "Check for an egress NetworkPolicy that doesn't allow port 53, and that CoreDNS pods in kube-system are ready.",
        ));
    } else if let Some(x) = ok_runs.iter().filter_map(|r| get(r, "service name")).find(|x| x.result != "ok") {
        out.push(f(
            "critical",
            format!("The service name for {svc} doesn't resolve"),
            format!("Cluster DNS works, but {} didn't resolve: {}", x.target, x.detail),
            "Check the service still exists under that name, and that clients use the right namespace. If your cluster domain isn't cluster.local, set it in Settings.",
        ));
    }

    // Service IP vs pod IPs, per probe (kube-proxy).
    for r in &ok_runs {
        let svc_checks: Vec<CheckResult> =
            r.results.iter().filter(|x| x.label.starts_with("service IP")).cloned().collect();
        let pod_ok = r.results.iter().any(|x| x.label.starts_with("pod ") && x.kind == "tcp" && x.result == "ok");
        for s in svc_checks.iter().filter(|x| x.result != "ok") {
            if pod_ok {
                out.push(f(
                    "critical",
                    format!("The service IP isn't forwarding to healthy pods{}", r.node.as_ref().map(|n| format!(" on node {n}")).unwrap_or_default()),
                    format!("Pod IPs answered directly, but {} ({}) gave {}. That points at kube-proxy (or the eBPF replacement) on this node not programming the service.", s.label, s.target, s.result),
                    "Check the kube-proxy pod on this node and its logs for sync errors (on Cilium, `cilium status` and its service list). Restarting kube-proxy on the node usually resyncs rules.",
                ));
            }
        }
    }

    // Pod reachability: same-node vs other-node (CNI), timeouts vs refusals.
    let pod_labels: Vec<String> = plan
        .checks
        .iter()
        .filter(|c| c.kind == "tcp" && c.label.starts_with("pod "))
        .map(|c| c.label.clone())
        .collect();
    for label in &pod_labels {
        let results: Vec<(&ProbeRun, CheckResult)> =
            ok_runs.iter().filter_map(|r| get(r, label).map(|x| (*r, x))).collect();
        if results.iter().all(|(_, x)| x.result == "ok") {
            continue;
        }
        let target_node = node_of(label);
        let from_same = results.iter().find(|(r, _)| r.node.is_some() && r.node == target_node);
        let from_other = results.iter().find(|(r, _)| r.node.is_some() && r.node != target_node);
        let target = &results[0].1.target;
        match (from_same, from_other) {
            (Some((_, a)), Some((ro, b))) if a.result == "ok" && b.result == "timeout" => out.push(f(
                "critical",
                format!("Cross-node traffic to {target} is dropped"),
                format!(
                    "{label} answers from its own node ({}) but times out from {}. Pod networking between nodes is broken, which is a CNI or node firewall problem, not the app.",
                    target_node.clone().unwrap_or_default(),
                    ro.node.clone().unwrap_or_default()
                ),
                "Check the CNI agent pods on both nodes (aws-node, calico-node, cilium) and that node security groups allow node-to-node traffic, including the overlay port if you use one (VXLAN UDP 4789 or 8472).",
            )),
            _ => {
                let worst = results.iter().map(|(_, x)| x.result.as_str()).find(|r| *r != "ok").unwrap_or("fail");
                if worst == "refused" {
                    out.push(f(
                        "critical",
                        format!("Nothing is listening on {target}"),
                        format!("{label}: the pod is reachable but refused the connection, so the app isn't listening on that port."),
                        "Check which port the app actually listens on, and that it binds to 0.0.0.0 rather than 127.0.0.1. Then align the service's targetPort.",
                    ));
                } else {
                    out.push(f(
                        "critical",
                        format!("Connections to {target} time out"),
                        format!(
                            "{label}: packets are dropped before reaching the app{}.",
                            if plan.policies_apply { ", and a NetworkPolicy selects this pod" } else { "" }
                        ),
                        if plan.policies_apply {
                            "Review the NetworkPolicies on the target pods; one probably doesn't admit traffic from this namespace on this port."
                        } else {
                            "With no NetworkPolicy involved, check security groups for pods (if used) and the CNI agent on the target's node."
                        },
                    ));
                }
            }
        }
    }

    // The pods' own health endpoints.
    for c in plan.checks.iter().filter(|c| c.kind == "http") {
        let Some(x) = ok_runs
            .iter()
            .filter_map(|r| get(r, &c.label))
            .find(|x| !(x.result.starts_with('2') || x.result.starts_with('3')))
        else {
            continue;
        };
        let (title, suggestion) = if x.result == "none" {
            (
                format!("{} didn't answer within its {}s timeout", c.label, c.timeout),
                "The endpoint is slower than the probe allows, so the kubelet will mark the pod unready or restart it. Make the endpoint cheaper, or raise timeoutSeconds.",
            )
        } else {
            (
                format!("{} returns HTTP {}", c.label, x.result),
                "The kubelet treats anything outside 200-399 as a failure. Fix the endpoint or point the probe at a path that reports health.",
            )
        };
        out.push(f("warning", title, format!("GET {} took {}.", x.target, x.detail), suggestion));
    }

    if !out.iter().any(|x| x.status != "ok") {
        out.push(f(
            "ok",
            format!("Every check to {svc} passed from {} {}", ok_runs.len(), if ok_runs.len() == 1 { "node" } else { "nodes" }),
            "DNS, the service IP, every tested pod and their health endpoints all responded.".into(),
            "If users still see errors, look at the entry point (ingress, load balancer target health) or at application-level errors in the logs.",
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cr(label: &str, result: &str) -> CheckResult {
        CheckResult {
            kind: if label.contains("probe") {
                "http"
            } else if label.contains("DNS") || label == "service name" {
                "dns"
            } else {
                "tcp"
            }
            .into(),
            label: label.into(),
            target: "10.0.1.5:8080".into(),
            result: result.into(),
            detail: "0s".into(),
        }
    }

    fn plan_with(policies: bool) -> TestPlan {
        let chk = |kind: &str, label: &str, node: Option<&str>| PlannedCheck {
            kind: kind.into(),
            label: label.into(),
            host: "10.0.1.5".into(),
            port: Some(8080),
            path: None,
            timeout: 3,
            target_node: node.map(String::from),
        };
        TestPlan {
            service: Target { kind: "Service".into(), namespace: "web".into(), name: "shop".into() },
            source_namespace: "web".into(),
            image: DEFAULT_IMAGE.into(),
            checks: vec![
                chk("dns", "cluster DNS", None),
                chk("tcp", "service IP port 80", None),
                chk("tcp", "pod shop-1 port 8080", Some("node-a")),
            ],
            probes: vec![],
            policies_apply: policies,
            notes: vec![],
        }
    }

    fn run(node: &str, results: Vec<CheckResult>) -> ProbeRun {
        ProbeRun {
            node: Some(node.into()),
            placement: if node == "node-a" { "same-node" } else { "other-node" }.into(),
            results,
            error: None,
        }
    }

    #[test]
    fn parses_probe_output() {
        let r = parse_output("R|dns|cluster DNS|kubernetes.default.svc.cluster.local|ok|10.100.0.1\nnoise\nR|tcp|pod a port 8080|10.0.1.5:8080|timeout|3s\nDONE\n");
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].result, "timeout");
    }

    #[test]
    fn kube_proxy_fault() {
        let runs = vec![run(
            "node-a",
            vec![cr("cluster DNS", "ok"), cr("service IP port 80", "timeout"), cr("pod shop-1 port 8080", "ok")],
        )];
        let f = analyze(&plan_with(false), &runs);
        assert!(f.iter().any(|x| x.title.contains("service IP isn't forwarding")), "{f:?}");
    }

    #[test]
    fn cni_cross_node() {
        let runs = vec![
            run(
                "node-a",
                vec![cr("cluster DNS", "ok"), cr("service IP port 80", "ok"), cr("pod shop-1 port 8080", "ok")],
            ),
            run(
                "node-b",
                vec![cr("cluster DNS", "ok"), cr("service IP port 80", "ok"), cr("pod shop-1 port 8080", "timeout")],
            ),
        ];
        let f = analyze(&plan_with(false), &runs);
        assert!(f.iter().any(|x| x.title.contains("Cross-node")), "{f:?}");
    }

    #[test]
    fn refused_means_app_not_listening() {
        let runs = vec![run(
            "node-a",
            vec![cr("cluster DNS", "ok"), cr("service IP port 80", "refused"), cr("pod shop-1 port 8080", "refused")],
        )];
        let f = analyze(&plan_with(false), &runs);
        assert!(f.iter().any(|x| x.title.contains("Nothing is listening")), "{f:?}");
        assert!(!f.iter().any(|x| x.title.contains("service IP isn't forwarding")));
    }

    #[test]
    fn timeouts_with_policy() {
        let runs = vec![run(
            "node-a",
            vec![cr("cluster DNS", "ok"), cr("service IP port 80", "timeout"), cr("pod shop-1 port 8080", "timeout")],
        )];
        let f = analyze(&plan_with(true), &runs);
        assert!(f.iter().any(|x| x.suggestion.contains("NetworkPolic")), "{f:?}");
    }

    #[test]
    fn all_good() {
        let runs = vec![run(
            "node-a",
            vec![cr("cluster DNS", "ok"), cr("service IP port 80", "ok"), cr("pod shop-1 port 8080", "ok")],
        )];
        let f = analyze(&plan_with(false), &runs);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].status, "ok");
    }

    #[test]
    fn script_quotes_and_refuses_unsafe_values() {
        assert!(!safe_path("/health; rm -rf /"));
        assert!(!safe_host("$(reboot)"));
        assert!(safe_path("/healthz?full=1"));
        let s = script_for(&[PlannedCheck {
            kind: "http".into(),
            label: "readiness probe /healthz on pod a".into(),
            host: "10.0.0.1".into(),
            port: Some(8080),
            path: Some("/healthz".into()),
            timeout: 2,
            target_node: None,
        }]);
        assert!(s.contains("http 'readiness probe /healthz on pod a' '10.0.0.1' 8080 2 '/healthz'"));
    }

    #[test]
    fn manifest_is_locked_down() {
        let m = manifest("web", Some("node-a"), DEFAULT_IMAGE, "echo hi");
        let pod: Pod = serde_json::from_value(m).unwrap();
        let spec = pod.spec.unwrap();
        assert_eq!(spec.automount_service_account_token, Some(false));
        assert_eq!(spec.active_deadline_seconds, Some(90));
        let sc = spec.containers[0].security_context.clone().unwrap();
        assert_eq!(sc.allow_privilege_escalation, Some(false));
        assert_eq!(sc.capabilities.unwrap().drop.unwrap(), vec!["ALL"]);
    }
}
