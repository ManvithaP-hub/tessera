//! Rule-based diagnosis over a [`ClusterGraph`].
//!
//! Each rule looks at one layer of the traffic path (entry, service,
//! workload, pod, node) and emits an [`Issue`] with evidence, a suggested fix
//! and read-only commands to confirm it. Pod findings are grouped per
//! workload so a 20-replica crash loop is one issue, not twenty.

use crate::model::*;
use crate::quantity::{fmt_bytes, fmt_cpu};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub fn diagnose(g: &ClusterGraph) -> Vec<Issue> {
    let mut out = Vec::new();
    entry_rules(g, &mut out);
    let pod_issue_pods = pod_rules(g, &mut out);
    service_rules(g, &pod_issue_pods, &mut out);
    node_rules(g, &mut out);
    workload_rules(g, &pod_issue_pods, &mut out);
    out.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then(a.layer.cmp(&b.layer))
            .then(a.target.namespace.cmp(&b.target.namespace))
            .then(a.target.name.cmp(&b.target.name))
    });
    out
}

fn target(kind: &str, ns: &str, name: &str) -> Target {
    Target { kind: kind.into(), namespace: ns.into(), name: name.into() }
}

fn fmt_selector(s: &BTreeMap<String, String>) -> String {
    s.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(",")
}

/* ---------------- Entry ---------------- */

fn entry_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    let services: BTreeSet<(&str, &str)> = g.services.iter().map(|s| (s.namespace.as_str(), s.name.as_str())).collect();
    for ing in &g.ingresses {
        let mut missing: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for r in &ing.routes {
            if !services.contains(&(ing.namespace.as_str(), r.service.as_str())) {
                missing.entry(&r.service).or_default().push(format!(
                    "{}{}",
                    r.host.clone().unwrap_or_else(|| "*".into()),
                    r.path
                ));
            }
        }
        for (svc, paths) in missing {
            let existing: Vec<&str> =
                g.services.iter().filter(|s| s.namespace == ing.namespace).map(|s| s.name.as_str()).take(8).collect();
            out.push(Issue {
                id: format!("entry-missing-backend:{}/{}/{}", ing.namespace, ing.name, svc),
                severity: Severity::Critical,
                layer: Layer::Entry,
                target: target("Ingress", &ing.namespace, &ing.name),
                title: format!("Ingress routes to a service that doesn't exist: {svc}"),
                detail: format!(
                    "{} sends {} to service {svc}, but there is no service with that name in {}. Requests on these routes fail at the ingress controller.",
                    ing.name,
                    paths.join(", "),
                    ing.namespace
                ),
                evidence: vec![
                    format!("backend service: {svc}"),
                    format!(
                        "services in {}: {}",
                        ing.namespace,
                        if existing.is_empty() { "<none>".to_string() } else { existing.join(", ") }
                    ),
                ],
                suggestion: format!(
                    "Create service {svc} in {}, or change the ingress backend to one of the existing services.",
                    ing.namespace
                ),
                commands: vec![
                    format!("kubectl get ingress {} -n {} -o yaml", ing.name, ing.namespace),
                    format!("kubectl get svc -n {}", ing.namespace),
                ],
                pods: vec![],
            });
        }
    }
}

/* ---------------- Service ---------------- */

fn service_rules(g: &ClusterGraph, pod_issue_pods: &BTreeSet<String>, out: &mut Vec<Issue>) {
    for s in &g.services {
        if s.selector.is_empty() || s.type_ == "ExternalName" {
            continue;
        }
        let sel = fmt_selector(&s.selector);
        let cmds = vec![
            format!("kubectl describe svc {} -n {}", s.name, s.namespace),
            format!("kubectl get pods -n {} -l {}", s.namespace, sel),
        ];
        if s.pods.is_empty() {
            // A workload whose template matches but has no pods is scaled to zero.
            if let Some(w) = g.workloads.iter().find(|w| s.workloads.contains(&w.id)) {
                out.push(Issue {
                    id: format!("svc-scaled-zero:{}/{}", s.namespace, s.name),
                    severity: Severity::Warning,
                    layer: Layer::Service,
                    target: target("Service", &s.namespace, &s.name),
                    title: format!("Service {} has no pods: {} is scaled to 0", s.name, w.name),
                    detail: format!(
                        "The selector matches {} {}, which currently runs no pods, so the service has no endpoints.",
                        w.kind, w.name
                    ),
                    evidence: vec![format!("selector: {sel}"), format!("{}: desired {}", w.id, w.desired)],
                    suggestion: format!("Scale {} up if this service should be serving traffic.", w.name),
                    commands: cmds,
                    pods: vec![],
                });
                continue;
            }
            // Find workloads in the namespace that carry the same label keys.
            let candidates: Vec<&WorkloadInfo> = g
                .workloads
                .iter()
                .filter(|w| w.namespace == s.namespace && s.selector.keys().all(|k| w.pod_labels.contains_key(k)))
                .collect();
            // Rank candidates: same name as the service, label values that
            // resemble the selector, and not already served by another service.
            let score = |w: &WorkloadInfo| {
                let mut sc = 0;
                if w.name == s.name {
                    sc += 3;
                }
                if s.selector.iter().any(|(k, v)| {
                    w.pod_labels
                        .get(k)
                        .is_some_and(|wv| wv != v && (v.contains(wv.as_str()) || wv.contains(v.as_str())))
                }) {
                    sc += 2;
                }
                if !g.services.iter().any(|o| o.workloads.contains(&w.id)) {
                    sc += 1;
                }
                sc
            };
            let mut ranked: Vec<(&WorkloadInfo, i32)> = candidates.iter().map(|w| (*w, score(w))).collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1));
            let best = match ranked.as_slice() {
                [(w, sc), rest @ ..] if *sc >= 2 && rest.first().is_none_or(|r| r.1 < *sc) => Some(*w),
                _ => None,
            };
            let labels_for = |w: &WorkloadInfo| -> BTreeMap<String, String> {
                s.selector.keys().filter_map(|k| w.pod_labels.get(k).map(|v| (k.clone(), v.clone()))).collect()
            };
            let mut evidence = vec![format!("selector: {sel}"), "endpoints: <none>".into()];
            let shown: Vec<&WorkloadInfo> = match best {
                Some(w) => vec![w],
                None => ranked.iter().take(3).map(|(w, _)| *w).collect(),
            };
            for w in shown {
                evidence.push(format!("{} pods are labelled {}", w.name, fmt_selector(&labels_for(w))));
            }
            let suggestion = match best {
                Some(w) => format!(
                    "If this service is meant for {}, change its selector to {}.",
                    w.name,
                    fmt_selector(&labels_for(w))
                ),
                None => {
                    "Compare the selector with the labels on the pods this service should reach, and make them match."
                        .into()
                }
            };
            out.push(Issue {
                id: format!("svc-no-match:{}/{}", s.namespace, s.name),
                severity: Severity::Critical,
                layer: Layer::Service,
                target: target("Service", &s.namespace, &s.name),
                title: format!("Service {} selects no pods", s.name),
                detail: format!(
                    "No pod in {} has the labels {sel}, so the service has no endpoints and anything routed to it gets connection errors or 503s.",
                    s.namespace
                ),
                evidence,
                suggestion,
                commands: cmds,
                pods: vec![],
            });
            continue;
        }
        if s.ready_endpoints == 0 {
            let explained = s.pods.iter().any(|p| pod_issue_pods.contains(&format!("{}/{}", s.namespace, p)));
            out.push(Issue {
                id: format!("svc-no-ready:{}/{}", s.namespace, s.name),
                severity: if explained { Severity::Warning } else { Severity::Critical },
                layer: Layer::Service,
                target: target("Service", &s.namespace, &s.name),
                title: format!("Service {} has no ready endpoints", s.name),
                detail: if explained {
                    format!(
                        "The selector matches {} pods, but none are ready. See the pod issues below for the cause.",
                        s.pods.len()
                    )
                } else {
                    format!("The selector matches {} pods, but none pass their readiness checks.", s.pods.len())
                },
                evidence: vec![
                    format!("selector: {sel}"),
                    format!("ready endpoints: 0, not ready: {}", s.not_ready_endpoints),
                ],
                suggestion: "Check the readiness probe and the pods' recent events.".into(),
                commands: cmds,
                pods: s.pods.clone(),
            });
        }
    }
}

/* ---------------- Pods ---------------- */

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PodFinding {
    OomKilled,
    CrashLoop,
    ImagePull,
    ConfigError,
    Unschedulable,
    NotReady,
}

/// Returns the set of `namespace/pod` keys that have a pod-level finding.
fn pod_rules(g: &ClusterGraph, out: &mut Vec<Issue>) -> BTreeSet<String> {
    let mut groups: BTreeMap<(String, PodFinding), Vec<&PodInfo>> = BTreeMap::new();
    let mut flagged = BTreeSet::new();
    for p in &g.pods {
        if let Some(f) = classify(p) {
            let key = p.workload.clone().unwrap_or_else(|| format!("Pod/{}/{}", p.namespace, p.name));
            groups.entry((key, f)).or_default().push(p);
            flagged.insert(format!("{}/{}", p.namespace, p.name));
        }
    }
    let largest_cpu = g.nodes.iter().map(|n| n.cpu_allocatable_milli).max().unwrap_or(0);
    let largest_mem = g.nodes.iter().map(|n| n.memory_allocatable_bytes).max().unwrap_or(0);

    for ((key, finding), pods) in groups {
        let first = pods[0];
        let (tkind, tns, tname) = split_id(&key);
        let who = if tkind == "Pod" { format!("Pod {tname}") } else { format!("{tkind} {tname}") };
        let names: Vec<String> = pods.iter().map(|p| p.name.clone()).collect();
        let count = if pods.len() == 1 { "1 pod".to_string() } else { format!("{} pods", pods.len()) };
        let ns = first.namespace.as_str();
        let base = Issue {
            id: String::new(),
            severity: Severity::Critical,
            layer: Layer::Pod,
            target: target(tkind, tns, tname),
            title: String::new(),
            detail: String::new(),
            evidence: vec![],
            suggestion: String::new(),
            commands: vec![format!("kubectl describe pod {} -n {ns}", first.name)],
            pods: names,
        };
        let issue = match finding {
            PodFinding::OomKilled => {
                let c = first
                    .containers
                    .iter()
                    .find(|c| c.last_reason.as_deref() == Some("OOMKilled") || c.reason.as_deref() == Some("OOMKilled"))
                    .expect("classified as OOMKilled");
                let restarts: i32 = pods.iter().map(|p| p.restarts).sum();
                let (detail, suggestion) = match c.memory_limit_bytes {
                    Some(l) => (
                        format!(
                            "Container {} in {count} was killed for using more than its {} memory limit, then restarted.",
                            c.name,
                            fmt_bytes(l)
                        ),
                        format!(
                            "Raise the memory limit for {} (for example to {}) and set the request to match, or find what grew the process's memory.",
                            c.name,
                            fmt_bytes(l * 2)
                        ),
                    ),
                    None => (
                        format!(
                            "Container {} in {count} was OOMKilled without a memory limit, which means the node itself ran low on memory.",
                            c.name
                        ),
                        format!("Set a memory request and limit on {} so the scheduler can place it safely.", c.name),
                    ),
                };
                let mut ev = vec![
                    format!("container: {}", c.name),
                    "last state: Terminated, reason OOMKilled, exit code 137".to_string(),
                    format!("restarts across affected pods: {restarts}"),
                ];
                if let Some(l) = c.memory_limit_bytes {
                    ev.push(format!("memory limit: {}", fmt_bytes(l)));
                }
                let mut commands = base.commands.clone();
                commands.push(format!("kubectl logs {} -n {ns} -c {} --previous", first.name, c.name));
                Issue {
                    id: format!("pod-oom:{key}"),
                    title: format!("{who} is running out of memory"),
                    detail,
                    evidence: ev,
                    suggestion,
                    commands,
                    ..base
                }
            }
            PodFinding::CrashLoop => {
                let c = first
                    .containers
                    .iter()
                    .find(|c| c.reason.as_deref() == Some("CrashLoopBackOff"))
                    .expect("classified as CrashLoopBackOff");
                let code = c.last_exit_code;
                let hint = match code {
                    Some(1) => "Exit code 1 usually means the application hit an error on start-up.",
                    Some(126) => {
                        "Exit code 126: the command exists but can't be executed (permissions or architecture)."
                    }
                    Some(127) => "Exit code 127: the command or entrypoint wasn't found in the image.",
                    Some(137) => "Exit code 137: the process was killed, often by a failing liveness probe.",
                    Some(139) => "Exit code 139: the process crashed with a segmentation fault.",
                    Some(143) => "Exit code 143: the process was stopped with SIGTERM.",
                    _ => "The container keeps exiting shortly after it starts.",
                };
                let mut commands = base.commands.clone();
                commands.push(format!("kubectl logs {} -n {ns} -c {} --previous", first.name, c.name));
                Issue {
                    id: format!("pod-crashloop:{key}"),
                    title: format!("{who} is crash looping"),
                    detail: format!("Container {} in {count} keeps exiting and restarting. {hint}", c.name),
                    evidence: vec![
                        format!("container: {}", c.name),
                        format!(
                            "last state: Terminated, reason {}, exit code {}",
                            c.last_reason.clone().unwrap_or_else(|| "unknown".into()),
                            code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into())
                        ),
                        format!("restart count: {}", c.restart_count),
                    ],
                    suggestion: "Read the previous container's logs to see why it exited.".into(),
                    commands,
                    ..base
                }
            }
            PodFinding::ImagePull => {
                let c = first
                    .containers
                    .iter()
                    .find(|c| is_image_pull(c.reason.as_deref()))
                    .expect("classified as image pull");
                let msg = c.message.clone().unwrap_or_default();
                let lower = msg.to_lowercase();
                let suggestion = if lower.contains("not found") || lower.contains("manifest unknown") {
                    format!("The tag in {} doesn't exist in the registry. Fix the tag or push the image, then roll out again.", c.image)
                } else if lower.contains("unauthorized") || lower.contains("denied") || lower.contains("403") {
                    "The node can't authenticate to the registry. Check imagePullSecrets or the node's registry permissions.".to_string()
                } else {
                    "Check the image name and tag, and that nodes can reach and authenticate to the registry."
                        .to_string()
                };
                let mut ev =
                    vec![format!("image: {}", c.image), format!("reason: {}", c.reason.clone().unwrap_or_default())];
                if !msg.is_empty() {
                    ev.push(format!("message: {msg}"));
                }
                Issue {
                    id: format!("pod-imagepull:{key}"),
                    title: format!("{who} can't pull its image"),
                    detail: format!("{count} can't start because the image for container {} can't be pulled.", c.name),
                    evidence: ev,
                    suggestion,
                    ..base
                }
            }
            PodFinding::ConfigError => {
                let c = first
                    .containers
                    .iter()
                    .find(|c| c.reason.as_deref() == Some("CreateContainerConfigError"))
                    .expect("classified as config error");
                Issue {
                    id: format!("pod-config:{key}"),
                    title: format!("{who} can't create its container"),
                    detail: format!(
                        "Container {} in {count} references configuration that doesn't exist or can't be read.",
                        c.name
                    ),
                    evidence: vec![format!("message: {}", c.message.clone().unwrap_or_default())],
                    suggestion: "Create the missing ConfigMap or Secret, or fix the key it references.".into(),
                    ..base
                }
            }
            PodFinding::Unschedulable => {
                let ev = scheduling_event(g, first);
                let msg = ev.map(|e| e.message.clone()).unwrap_or_default();
                let mut evidence = vec![];
                if !msg.is_empty() {
                    evidence.push(format!("FailedScheduling: {msg}"));
                }
                let cpu_req: i64 = first.containers.iter().filter_map(|c| c.cpu_request_milli).sum();
                let wl = g.workloads.iter().find(|w| Some(&w.id) == first.workload.as_ref());
                let mem_req: i64 =
                    wl.map(|w| w.containers.iter().filter_map(|c| c.memory_request_bytes).sum()).unwrap_or(0);
                let suggestion = if msg.contains("Insufficient cpu") {
                    if cpu_req > 0 && largest_cpu > 0 {
                        evidence.push(format!(
                            "requested cpu: {}, largest node allocatable: {}",
                            fmt_cpu(cpu_req),
                            fmt_cpu(largest_cpu)
                        ));
                    }
                    if cpu_req > largest_cpu && largest_cpu > 0 {
                        format!(
                            "No node is big enough for a {} CPU request. Lower the request or add a node group with larger instances.",
                            fmt_cpu(cpu_req)
                        )
                    } else {
                        "Nodes are full on CPU. Lower requests, remove idle workloads, or add nodes (check the cluster autoscaler).".into()
                    }
                } else if msg.contains("Insufficient memory") {
                    if mem_req > 0 && largest_mem > 0 {
                        evidence.push(format!(
                            "requested memory: {}, largest node allocatable: {}",
                            fmt_bytes(mem_req),
                            fmt_bytes(largest_mem)
                        ));
                    }
                    "Nodes don't have enough free memory for this request. Lower it or add capacity.".into()
                } else if msg.contains("untolerated taint") {
                    "The pod has no toleration for the taints on the available nodes. Add a toleration or schedule it onto untainted nodes.".into()
                } else if msg.contains("affinity") || msg.contains("node selector") {
                    "No node matches the pod's nodeSelector or affinity rules. Check the labels on your nodes.".into()
                } else if msg.contains("PersistentVolumeClaim") {
                    "The pod is waiting on a PersistentVolumeClaim that isn't bound. Check the claim and its storage class.".into()
                } else if msg.is_empty() {
                    "The scheduler hasn't placed the pod yet and there's no scheduling event. Check the scheduler and the pod's events.".into()
                } else {
                    "Read the scheduler message above; it names what every node is missing.".into()
                };
                Issue {
                    id: format!("pod-unschedulable:{key}"),
                    layer: Layer::Node,
                    title: format!("{who} can't be scheduled"),
                    detail: format!("{count} stuck in Pending because no node can take them."),
                    evidence,
                    suggestion,
                    ..base
                }
            }
            PodFinding::NotReady => {
                let ev = g.events.iter().find(|e| {
                    e.kind == "Pod" && e.namespace == first.namespace && e.name == first.name && e.reason == "Unhealthy"
                });
                Issue {
                    id: format!("pod-notready:{key}"),
                    severity: Severity::Warning,
                    title: format!("{who} is failing readiness checks"),
                    detail: format!("{count} are running but not ready, so they receive no service traffic."),
                    evidence: ev.map(|e| vec![format!("Unhealthy: {}", e.message)]).unwrap_or_default(),
                    suggestion:
                        "Check the readiness probe's path, port and timing against what the app actually serves.".into(),
                    ..base
                }
            }
        };
        out.push(issue);
    }
    flagged
}

fn is_image_pull(r: Option<&str>) -> bool {
    matches!(r, Some("ImagePullBackOff") | Some("ErrImagePull") | Some("InvalidImageName") | Some("ErrImageNeverPull"))
}

fn classify(p: &PodInfo) -> Option<PodFinding> {
    if p.phase == "Succeeded" {
        return None;
    }
    let cs = &p.containers;
    // A container that was OOMKilled once and is healthy again is not flagged;
    // only one that is currently down because of it.
    if cs.iter().any(|c| {
        c.reason.as_deref() == Some("OOMKilled")
            || (c.reason.as_deref() == Some("CrashLoopBackOff") && c.last_reason.as_deref() == Some("OOMKilled"))
    }) {
        return Some(PodFinding::OomKilled);
    }
    if cs.iter().any(|c| c.reason.as_deref() == Some("CrashLoopBackOff")) {
        return Some(PodFinding::CrashLoop);
    }
    if cs.iter().any(|c| is_image_pull(c.reason.as_deref())) {
        return Some(PodFinding::ImagePull);
    }
    if cs.iter().any(|c| c.reason.as_deref() == Some("CreateContainerConfigError")) {
        return Some(PodFinding::ConfigError);
    }
    if p.phase == "Pending" && p.node.is_none() {
        return Some(PodFinding::Unschedulable);
    }
    if p.phase == "Running" && !p.ready && cs.iter().all(|c| c.state == "running") {
        return Some(PodFinding::NotReady);
    }
    None
}

fn scheduling_event<'a>(g: &'a ClusterGraph, p: &PodInfo) -> Option<&'a EventInfo> {
    g.events
        .iter()
        .find(|e| e.kind == "Pod" && e.namespace == p.namespace && e.name == p.name && e.reason == "FailedScheduling")
}

fn split_id(id: &str) -> (&str, &str, &str) {
    let mut it = id.splitn(3, '/');
    (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""))
}

/* ---------------- Nodes ---------------- */

fn node_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    let pods_on: HashMap<&str, usize> =
        g.pods.iter().filter_map(|p| p.node.as_deref()).fold(HashMap::new(), |mut m, n| {
            *m.entry(n).or_default() += 1;
            m
        });
    for n in &g.nodes {
        let cmds = vec![format!("kubectl describe node {}", n.name)];
        let on = pods_on.get(n.name.as_str()).copied().unwrap_or(0);
        if !n.ready {
            out.push(Issue {
                id: format!("node-notready:{}", n.name),
                severity: Severity::Critical,
                layer: Layer::Node,
                target: target("Node", "", &n.name),
                title: format!("Node {} is not ready", n.name),
                detail: format!("The kubelet on {} isn't reporting Ready. {on} pods are assigned to it.", n.name),
                evidence: vec!["condition Ready: not True".into()],
                suggestion: "Check the node's kubelet and network. Pods on it will be evicted if it stays NotReady."
                    .into(),
                commands: cmds.clone(),
                pods: vec![],
            });
        }
        if !n.pressure.is_empty() {
            out.push(Issue {
                id: format!("node-pressure:{}", n.name),
                severity: Severity::Warning,
                layer: Layer::Node,
                target: target("Node", "", &n.name),
                title: format!("Node {} reports {}", n.name, n.pressure.join(", ")),
                detail: "The kubelet may evict pods from this node until the pressure clears.".into(),
                evidence: n.pressure.iter().map(|c| format!("condition {c}: True")).collect(),
                suggestion: "Find the heaviest pods on the node and give them accurate requests and limits.".into(),
                commands: cmds.clone(),
                pods: vec![],
            });
        }
        if n.unschedulable {
            out.push(Issue {
                id: format!("node-cordoned:{}", n.name),
                severity: Severity::Warning,
                layer: Layer::Node,
                target: target("Node", "", &n.name),
                title: format!("Node {} is cordoned", n.name),
                detail: "No new pods will be scheduled here until it is uncordoned.".into(),
                evidence: vec!["spec.unschedulable: true".into()],
                suggestion: "If maintenance is finished, uncordon the node.".into(),
                commands: cmds,
                pods: vec![],
            });
        }
    }
}

/* ---------------- Workloads ---------------- */

fn workload_rules(g: &ClusterGraph, pod_issue_pods: &BTreeSet<String>, out: &mut Vec<Issue>) {
    for w in &g.workloads {
        if w.desired == 0 || w.ready >= w.desired {
            continue;
        }
        let explained = g.pods.iter().any(|p| {
            p.workload.as_ref() == Some(&w.id) && pod_issue_pods.contains(&format!("{}/{}", p.namespace, p.name))
        });
        if explained {
            continue;
        }
        out.push(Issue {
            id: format!("workload-unavailable:{}", w.id),
            severity: Severity::Warning,
            layer: Layer::Workload,
            target: target(&w.kind, &w.namespace, &w.name),
            title: format!("{} {} has {} of {} replicas ready", w.kind, w.name, w.ready, w.desired),
            detail: "None of its pods show a specific failure, so this is often a rollout in progress or pods still starting.".into(),
            evidence: vec![format!("desired {}, ready {}, available {}", w.desired, w.ready, w.available)],
            suggestion: "Watch the rollout. If it doesn't converge, check the pods' events.".into(),
            commands: vec![format!("kubectl rollout status {}/{} -n {}", w.kind.to_lowercase(), w.name, w.namespace)],
            pods: vec![],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn workload(ns: &str, name: &str, desired: i32, ready: i32) -> WorkloadInfo {
        WorkloadInfo {
            id: workload_id("Deployment", ns, name),
            kind: "Deployment".into(),
            namespace: ns.into(),
            name: name.into(),
            desired,
            ready,
            available: ready,
            pod_labels: labels(&[("app", name)]),
            containers: vec![],
        }
    }

    fn pod(ns: &str, name: &str, wl: &str) -> PodInfo {
        PodInfo {
            namespace: ns.into(),
            name: name.into(),
            status: "Running".into(),
            phase: "Running".into(),
            ready: true,
            node: Some("node-a".into()),
            workload: Some(workload_id("Deployment", ns, wl)),
            labels: labels(&[("app", wl)]),
            containers: vec![ContainerStatusInfo {
                name: wl.into(),
                image: format!("{wl}:1"),
                ready: true,
                state: "running".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn node(name: &str, cpu: i64) -> NodeInfo {
        NodeInfo {
            name: name.into(),
            ready: true,
            cpu_allocatable_milli: cpu,
            memory_allocatable_bytes: 16 << 30,
            ..Default::default()
        }
    }

    #[test]
    fn selector_mismatch_names_the_fix() {
        let g = ClusterGraph {
            workloads: vec![workload("web", "catalog", 2, 2)],
            pods: vec![pod("web", "catalog-1", "catalog")],
            services: vec![ServiceInfo {
                namespace: "web".into(),
                name: "catalog".into(),
                type_: "ClusterIP".into(),
                selector: labels(&[("app", "catalog-svc")]),
                ..Default::default()
            }],
            ..Default::default()
        };
        let issues = diagnose(&g);
        assert_eq!(issues.len(), 1);
        let i = &issues[0];
        assert_eq!(i.layer, Layer::Service);
        assert_eq!(i.severity, Severity::Critical);
        assert!(i.suggestion.contains("app=catalog."), "{}", i.suggestion);
    }

    #[test]
    fn oom_is_grouped_per_workload() {
        let mut p1 = pod("pay", "api-1", "api");
        let mut p2 = pod("pay", "api-2", "api");
        for p in [&mut p1, &mut p2] {
            p.ready = false;
            p.status = "CrashLoopBackOff".into();
            p.restarts = 5;
            let c = &mut p.containers[0];
            c.state = "waiting".into();
            c.reason = Some("CrashLoopBackOff".into());
            c.last_reason = Some("OOMKilled".into());
            c.last_exit_code = Some(137);
            c.restart_count = 5;
            c.memory_limit_bytes = Some(256 << 20);
        }
        let g = ClusterGraph {
            workloads: vec![workload("pay", "api", 2, 0)],
            pods: vec![p1, p2],
            services: vec![ServiceInfo {
                namespace: "pay".into(),
                name: "api".into(),
                type_: "ClusterIP".into(),
                selector: labels(&[("app", "api")]),
                pods: vec!["api-1".into(), "api-2".into()],
                workloads: vec![workload_id("Deployment", "pay", "api")],
                ..Default::default()
            }],
            ..Default::default()
        };
        let issues = diagnose(&g);
        let oom: Vec<_> = issues.iter().filter(|i| i.id.starts_with("pod-oom")).collect();
        assert_eq!(oom.len(), 1);
        assert_eq!(oom[0].pods.len(), 2);
        assert!(oom[0].suggestion.contains("512Mi"));
        // The service symptom is downgraded because the pods explain it.
        let svc = issues.iter().find(|i| i.layer == Layer::Service).unwrap();
        assert_eq!(svc.severity, Severity::Warning);
        // The workload rule stays quiet because pods explain it.
        assert!(!issues.iter().any(|i| i.layer == Layer::Workload));
    }

    #[test]
    fn unschedulable_compares_to_largest_node() {
        let mut p = pod("ml", "rec-1", "rec");
        p.phase = "Pending".into();
        p.status = "Pending".into();
        p.ready = false;
        p.node = None;
        p.containers[0].state = "waiting".into();
        p.containers[0].cpu_request_milli = Some(6000);
        let g = ClusterGraph {
            nodes: vec![node("node-a", 3920), node("node-b", 3920)],
            workloads: vec![workload("ml", "rec", 1, 0)],
            pods: vec![p],
            events: vec![EventInfo {
                namespace: "ml".into(),
                kind: "Pod".into(),
                name: "rec-1".into(),
                type_: "Warning".into(),
                reason: "FailedScheduling".into(),
                message: "0/2 nodes are available: 2 Insufficient cpu.".into(),
                count: 3,
                last_seen: None,
            }],
            ..Default::default()
        };
        let issues = diagnose(&g);
        let i = issues.iter().find(|i| i.id.starts_with("pod-unschedulable")).unwrap();
        assert_eq!(i.layer, Layer::Node);
        assert!(i.evidence.iter().any(|e| e.contains("largest node allocatable: 3.92")));
        assert!(i.suggestion.contains("No node is big enough"));
    }

    #[test]
    fn missing_ingress_backend() {
        let g = ClusterGraph {
            ingresses: vec![IngressInfo {
                namespace: "web".into(),
                name: "edge".into(),
                routes: vec![Route {
                    host: Some("shop.example.com".into()),
                    path: "/api".into(),
                    service: "gone".into(),
                    port: None,
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let issues = diagnose(&g);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].layer, Layer::Entry);
    }

    #[test]
    fn healthy_cluster_has_no_issues() {
        let g = ClusterGraph {
            nodes: vec![node("node-a", 4000)],
            workloads: vec![workload("web", "shop", 1, 1)],
            pods: vec![pod("web", "shop-1", "shop")],
            services: vec![ServiceInfo {
                namespace: "web".into(),
                name: "shop".into(),
                type_: "ClusterIP".into(),
                selector: labels(&[("app", "shop")]),
                ready_endpoints: 1,
                pods: vec!["shop-1".into()],
                workloads: vec![workload_id("Deployment", "web", "shop")],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(diagnose(&g).is_empty());
    }
}
