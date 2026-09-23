//! Rules beyond the core request path: network policies, cluster DNS,
//! ingress and cloud load balancer setup, service ports, storage, admission
//! and quota, autoscaling, evictions and service mesh (Istio).
//!
//! Every rule here is conservative: when the API data can't prove a problem
//! (for example a NetworkPolicy using matchExpressions), it stays quiet rather
//! than guessing.

use crate::model::*;
use std::collections::{BTreeMap, BTreeSet};

pub fn run(g: &ClusterGraph, out: &mut Vec<Issue>) {
    network_policy_rules(g, out);
    dns_rules(g, out);
    ingress_setup_rules(g, out);
    load_balancer_rules(g, out);
    service_port_rules(g, out);
    storage_rules(g, out);
    admission_and_quota_rules(g, out);
    hpa_rules(g, out);
    eviction_rules(g, out);
    mesh_rules(g, out);
    lb_target_rules(g, out);
    cni_rules(g, out);
    probe_rules(g, out);
}

/* ---------------- helpers ---------------- */

fn target(kind: &str, ns: &str, name: &str) -> Target {
    Target { kind: kind.into(), namespace: ns.into(), name: name.into() }
}

#[allow(clippy::too_many_arguments)]
fn issue(
    id: String,
    severity: Severity,
    layer: Layer,
    t: Target,
    title: String,
    detail: String,
    evidence: Vec<String>,
    suggestion: String,
    commands: Vec<String>,
    pods: Vec<String>,
) -> Issue {
    Issue {
        id,
        severity,
        layer,
        category: Category::Other,
        target: t,
        title,
        detail,
        evidence,
        suggestion,
        commands,
        pods,
    }
}

/// Group pods by owning workload (or the pod itself when unowned).
fn group_by_owner<'a>(pods: impl Iterator<Item = &'a PodInfo>) -> BTreeMap<String, Vec<&'a PodInfo>> {
    let mut m: BTreeMap<String, Vec<&PodInfo>> = BTreeMap::new();
    for p in pods {
        let k = p.workload.clone().unwrap_or_else(|| workload_id("Pod", &p.namespace, &p.name));
        m.entry(k).or_default().push(p);
    }
    m
}

fn split_id(id: &str) -> (&str, &str, &str) {
    let mut it = id.splitn(3, '/');
    (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""))
}

fn owner_label(id: &str) -> String {
    let (k, _, n) = split_id(id);
    format!("{k} {n}")
}

fn np_selects(np: &NetworkPolicyInfo, p: &PodInfo) -> bool {
    np.namespace == p.namespace
        && !np.selector_complex
        && (np.pod_selector.is_empty() || labels_match(&np.pod_selector, &p.labels))
}

/// Does a policy port entry cover this numeric port / name / protocol?
fn port_covers(pp: &NpPort, port: i32, name: Option<&str>, protocol: &str) -> bool {
    if !pp.protocol.eq_ignore_ascii_case(protocol) {
        return false;
    }
    match &pp.port {
        None => true,
        Some(v) => match v.parse::<i32>() {
            Ok(n) => port == n || pp.end_port.is_some_and(|e| port >= n && port <= e),
            Err(_) => name == Some(v.as_str()),
        },
    }
}

fn rule_allows_port(r: &NpRule, port: i32, name: Option<&str>, protocol: &str) -> bool {
    r.ports.is_empty() || r.ports.iter().any(|pp| port_covers(pp, port, name, protocol))
}

fn rule_summary(r: &NpRule) -> String {
    let peers = if r.peers.is_empty() { "any source".to_string() } else { r.peers.join("; ") };
    let ports = if r.ports.is_empty() {
        "all ports".to_string()
    } else {
        r.ports
            .iter()
            .map(|p| format!("{}/{}", p.port.clone().unwrap_or_else(|| "*".into()), p.protocol))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("{peers} on {ports}")
}

/// Resolve a Service port's target to (number, name) for a given pod.
fn resolve_target(sp: &ServicePortInfo, p: &PodInfo) -> Option<(i32, Option<String>)> {
    match sp.target.parse::<i32>() {
        Ok(n) => Some((n, p.container_ports.iter().find(|c| c.port == n).and_then(|c| c.name.clone()))),
        Err(_) => p
            .container_ports
            .iter()
            .find(|c| c.name.as_deref() == Some(sp.target.as_str()))
            .map(|c| (c.port, c.name.clone())),
    }
}

/* ---------------- NetworkPolicy ---------------- */

fn network_policy_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    if g.network_policies.is_empty() {
        return;
    }
    let routed: BTreeSet<(String, String)> = g
        .ingresses
        .iter()
        .flat_map(|i| i.routes.iter().map(move |r| (i.namespace.clone(), r.service.clone())))
        .collect();

    // Ingress: does any policy block traffic to a Service's pods on its target port?
    for s in &g.services {
        if s.pods.is_empty() {
            continue;
        }
        let pods: Vec<&PodInfo> =
            g.pods.iter().filter(|p| p.namespace == s.namespace && s.pods.contains(&p.name)).collect();
        for sp in &s.ports_detail {
            let mut blocked = Vec::new();
            let mut restricted: Vec<String> = Vec::new();
            let mut policy_names = BTreeSet::new();
            for p in &pods {
                let pols: Vec<&NetworkPolicyInfo> =
                    g.network_policies.iter().filter(|np| np.ingress_type && np_selects(np, p)).collect();
                if pols.is_empty() {
                    continue;
                }
                pols.iter().for_each(|np| {
                    policy_names.insert(np.name.clone());
                });
                let Some((port, pname)) = resolve_target(sp, p) else { continue };
                let allowing: Vec<&NpRule> = pols
                    .iter()
                    .flat_map(|np| np.ingress.iter())
                    .filter(|r| rule_allows_port(r, port, pname.as_deref(), &sp.protocol))
                    .collect();
                if allowing.is_empty() {
                    blocked.push(p.name.clone());
                } else if !allowing.iter().any(|r| {
                    r.peers.is_empty() || r.peers.iter().any(|x| x == "all namespaces" || x.starts_with("ipBlock"))
                }) {
                    restricted.extend(allowing.iter().flat_map(|r| r.peers.clone()));
                }
            }
            let pols: Vec<String> = policy_names.into_iter().collect();
            let pol_rules: Vec<String> = g
                .network_policies
                .iter()
                .filter(|np| np.namespace == s.namespace && pols.contains(&np.name))
                .map(|np| {
                    if np.ingress.is_empty() {
                        format!("{}: ingress rules: none (denies all inbound)", np.name)
                    } else {
                        format!(
                            "{}: allows {}",
                            np.name,
                            np.ingress.iter().map(rule_summary).collect::<Vec<_>>().join(" | ")
                        )
                    }
                })
                .collect();
            let cmds = vec![
                format!("kubectl get networkpolicy -n {} -o yaml", s.namespace),
                format!("kubectl describe svc {} -n {}", s.name, s.namespace),
            ];
            if !blocked.is_empty() {
                let all = blocked.len() == pods.len();
                out.push(issue(
                    format!("np-ingress-blocked:{}/{}:{}", s.namespace, s.name, sp.port),
                    if all { Severity::Critical } else { Severity::Warning },
                    Layer::Pod,
                    target("Service", &s.namespace, &s.name),
                    format!("NetworkPolicy blocks traffic to {} on port {}", s.name, sp.target),
                    format!(
                        "{} of {} pods behind {} are selected by a NetworkPolicy that allows no inbound traffic on port {}. Connections are silently dropped, so callers see timeouts rather than errors.",
                        blocked.len(),
                        pods.len(),
                        s.name,
                        sp.target
                    ),
                    pol_rules,
                    format!(
                        "Add an ingress rule to one of these policies that allows port {}/{} from the callers of {} (for routed services, the ingress controller's namespace).",
                        sp.target, sp.protocol, s.name
                    ),
                    cmds,
                    blocked,
                ));
            } else if !restricted.is_empty() && routed.contains(&(s.namespace.clone(), s.name.clone())) {
                restricted.sort();
                restricted.dedup();
                out.push(issue(
                    format!("np-ingress-restricted:{}/{}:{}", s.namespace, s.name, sp.port),
                    Severity::Warning,
                    Layer::Pod,
                    target("Service", &s.namespace, &s.name),
                    format!("Only specific sources may reach {}; check the ingress controller is one of them", s.name),
                    format!(
                        "{} is routed from an ingress, but NetworkPolicies only admit the sources below on port {}. If the ingress controller's pods or namespace aren't included, requests through the ingress time out.",
                        s.name, sp.target
                    ),
                    restricted.iter().map(|r| format!("allowed from: {r}")).chain(pol_rules).collect(),
                    "Confirm the ingress controller's namespace (for example ingress-nginx) or, for AWS ALB in IP mode, the VPC CIDR is allowed.".into(),
                    cmds,
                    vec![],
                ));
            }
        }
    }

    // Egress: does a policy stop pods from reaching DNS (port 53)?
    let affected = g.pods.iter().filter(|p| p.phase == "Running" || p.phase == "Pending").filter(|p| {
        let pols: Vec<&NetworkPolicyInfo> =
            g.network_policies.iter().filter(|np| np.egress_type && np_selects(np, p)).collect();
        !pols.is_empty()
            && !pols
                .iter()
                .flat_map(|np| np.egress.iter())
                .any(|r| rule_allows_port(r, 53, Some("dns"), "UDP") || rule_allows_port(r, 53, Some("dns-tcp"), "TCP"))
    });
    for (owner, pods) in group_by_owner(affected) {
        let ns = &pods[0].namespace;
        let pols: Vec<&NetworkPolicyInfo> =
            g.network_policies.iter().filter(|np| np.egress_type && np_selects(np, pods[0])).collect();
        let deny_all = pols.iter().all(|np| np.egress.is_empty());
        let (k, _, n) = split_id(&owner);
        out.push(issue(
            format!("np-egress-dns:{owner}"),
            Severity::Critical,
            Layer::Pod,
            target(k, ns, n),
            if deny_all {
                format!("NetworkPolicy blocks all outbound traffic from {}, including DNS", owner_label(&owner))
            } else {
                format!("NetworkPolicy blocks DNS lookups from {}", owner_label(&owner))
            },
            "Egress policies select these pods but none allow port 53. Every hostname lookup fails, including other services in the cluster, so calls fail with name-resolution errors even though the targets are healthy.".into(),
            pols.iter()
                .map(|np| {
                    if np.egress.is_empty() {
                        format!("{}: egress rules: none (denies all outbound)", np.name)
                    } else {
                        format!("{}: allows {}", np.name, np.egress.iter().map(rule_summary).collect::<Vec<_>>().join(" | "))
                    }
                })
                .collect(),
            "Add an egress rule allowing UDP and TCP port 53 to the kube-system namespace (where CoreDNS runs).".into(),
            vec![
                format!("kubectl get networkpolicy -n {ns} -o yaml"),
                format!("kubectl exec -n {ns} {} -- nslookup kubernetes.default", pods[0].name),
            ],
            pods.iter().map(|p| p.name.clone()).collect(),
        ));
    }
}

/* ---------------- Cluster DNS ---------------- */

fn dns_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    if let Some(svc) = g.services.iter().find(|s| s.namespace == "kube-system" && s.name == "kube-dns") {
        if svc.ready_endpoints == 0 {
            out.push(issue(
                "dns-down:kube-system/kube-dns".into(),
                Severity::Critical,
                Layer::Service,
                target("Service", "kube-system", "kube-dns"),
                "Cluster DNS has no ready endpoints".into(),
                "The kube-dns service (served by CoreDNS) has no ready pods. Every pod that looks up a hostname, including other services, gets resolution failures. This breaks almost everything at once.".into(),
                vec![format!("kube-dns ready endpoints: 0, not ready: {}", svc.not_ready_endpoints)],
                "Check the CoreDNS pods in kube-system: their status, logs and whether they can be scheduled.".into(),
                vec![
                    "kubectl get pods -n kube-system -l k8s-app=kube-dns".into(),
                    "kubectl logs -n kube-system -l k8s-app=kube-dns --tail=50".into(),
                ],
                svc.pods.clone(),
            ));
            return;
        }
    }
    if let Some(w) = g
        .workloads
        .iter()
        .find(|w| w.namespace == "kube-system" && (w.name == "coredns" || w.name == "kube-dns") && w.ready < w.desired)
    {
        out.push(issue(
            format!("dns-degraded:{}", w.id),
            Severity::Warning,
            Layer::Workload,
            target(&w.kind, &w.namespace, &w.name),
            format!("Cluster DNS is degraded: {} of {} CoreDNS replicas ready", w.ready, w.desired),
            "Lookups still work but with less capacity; under load you may see intermittent resolution timeouts."
                .into(),
            vec![format!("{}: desired {}, ready {}", w.name, w.desired, w.ready)],
            "Find out why the missing CoreDNS replicas aren't ready.".into(),
            vec!["kubectl get pods -n kube-system -l k8s-app=kube-dns".into()],
            vec![],
        ));
    }
    let dns_cfg: Vec<&EventInfo> = g.events.iter().filter(|e| e.reason == "DNSConfigForming").collect();
    if let Some(e) = dns_cfg.first() {
        out.push(issue(
            format!("dns-config:{}/{}", e.namespace, e.name),
            Severity::Warning,
            Layer::Node,
            target(&e.kind, &e.namespace, &e.name),
            "Node DNS configuration is being truncated".into(),
            format!(
                "{} pods reported DNSConfigForming: the node's resolv.conf has more nameservers or search domains than Kubernetes allows, so some are dropped.",
                dns_cfg.len()
            ),
            vec![format!("DNSConfigForming: {}", e.message)],
            "Reduce nameservers (max 3) or search domains on the node, or set the pod's dnsConfig explicitly.".into(),
            vec![format!("kubectl describe {} {} -n {}", e.kind.to_lowercase(), e.name, e.namespace)],
            vec![],
        ));
    }
}

/* ---------------- Ingress setup ---------------- */

fn ingress_setup_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    for ing in &g.ingresses {
        let t = target("Ingress", &ing.namespace, &ing.name);
        let cmds = vec![
            format!("kubectl describe ingress {} -n {}", ing.name, ing.namespace),
            "kubectl get ingressclass".to_string(),
        ];
        let mut class_problem = false;
        if !g.ingress_classes.is_empty() {
            match &ing.class_name {
                Some(c) if !g.ingress_classes.contains(c) => {
                    class_problem = true;
                    out.push(issue(
                        format!("ingress-class-missing:{}/{}", ing.namespace, ing.name),
                        Severity::Critical,
                        Layer::Entry,
                        t.clone(),
                        format!("Ingress {} uses class {c}, which doesn't exist", ing.name),
                        "No ingress controller watches this class, so nothing will ever serve these routes.".into(),
                        vec![
                            format!("ingressClassName: {c}"),
                            format!("classes in cluster: {}", g.ingress_classes.join(", ")),
                        ],
                        format!("Set ingressClassName to one of: {}.", g.ingress_classes.join(", ")),
                        cmds.clone(),
                        vec![],
                    ));
                }
                None if g.default_ingress_class.is_none() => {
                    class_problem = true;
                    out.push(issue(
                        format!("ingress-class-none:{}/{}", ing.namespace, ing.name),
                        Severity::Warning,
                        Layer::Entry,
                        t.clone(),
                        format!("Ingress {} has no class and the cluster has no default class", ing.name),
                        "Unless a controller is configured to watch class-less ingresses, nothing serves it.".into(),
                        vec![format!("classes in cluster: {}", g.ingress_classes.join(", "))],
                        "Set spec.ingressClassName, or mark one IngressClass as the default.".into(),
                        cmds.clone(),
                        vec![],
                    ));
                }
                _ => {}
            }
        }
        if !class_problem && ing.addresses.is_empty() && !ing.routes.is_empty() {
            out.push(issue(
                format!("ingress-no-address:{}/{}", ing.namespace, ing.name),
                Severity::Warning,
                Layer::Entry,
                t.clone(),
                format!("Ingress {} has no address yet", ing.name),
                "The controller hasn't published an address for it. Either it is still provisioning a load balancer, or the controller isn't running or failed to reconcile it.".into(),
                vec!["status.loadBalancer.ingress: <empty>".into()],
                "Check the ingress controller's pods and logs, and the ingress's events for reconcile errors.".into(),
                cmds.clone(),
                vec![],
            ));
        }
        if let Some(secrets) = &g.secret_names {
            for sec in &ing.tls_secrets {
                if !secrets.contains(&format!("{}/{sec}", ing.namespace)) {
                    out.push(issue(
                        format!("ingress-tls-missing:{}/{}/{sec}", ing.namespace, ing.name),
                        Severity::Critical,
                        Layer::Entry,
                        t.clone(),
                        format!("TLS secret {sec} for ingress {} doesn't exist", ing.name),
                        "HTTPS on these hosts will fail or fall back to the controller's default certificate, which browsers reject.".into(),
                        vec![format!("spec.tls secretName: {sec}"), format!("namespace: {}", ing.namespace)],
                        "Create the secret (or check your cert-manager Certificate is Ready), in the same namespace as the ingress.".into(),
                        vec![format!("kubectl get secret {sec} -n {}", ing.namespace), format!("kubectl get certificate -n {}", ing.namespace)],
                        vec![],
                    ));
                }
            }
        }
    }
}

/* ---------------- Cloud load balancers ---------------- */

fn load_balancer_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    for s in g.services.iter().filter(|s| s.type_ == "LoadBalancer") {
        let failures: Vec<&EventInfo> = g
            .events
            .iter()
            .filter(|e| {
                e.kind == "Service"
                    && e.namespace == s.namespace
                    && e.name == s.name
                    && (e.reason.contains("LoadBalancerFailed")
                        || e.reason == "FailedDeployModel"
                        || e.reason == "FailedBuildModel")
            })
            .collect();
        let cmds = vec![format!("kubectl describe svc {} -n {}", s.name, s.namespace)];
        if let Some(e) = failures.first() {
            out.push(issue(
                format!("lb-failed:{}/{}", s.namespace, s.name),
                Severity::Critical,
                Layer::Entry,
                target("Service", &s.namespace, &s.name),
                format!("Cloud load balancer for {} failed to provision", s.name),
                "The cloud controller reported an error creating or updating the load balancer, so there is no working external entry point.".into(),
                failures.iter().take(3).map(|e| format!("{}: {}", e.reason, e.message)).collect(),
                if e.message.contains("subnet") {
                    "Check that your subnets are tagged for load balancers (on AWS: kubernetes.io/role/elb or internal-elb).".into()
                } else if e.message.to_lowercase().contains("quota") || e.message.contains("LimitExceeded") {
                    "The cloud account hit a load balancer quota. Remove unused load balancers or request a higher limit.".into()
                } else {
                    "Read the controller error above; it usually names the missing permission, subnet or annotation.".into()
                },
                cmds,
                vec![],
            ));
        } else if s.external.is_empty() {
            out.push(issue(
                format!("lb-pending:{}/{}", s.namespace, s.name),
                Severity::Warning,
                Layer::Entry,
                target("Service", &s.namespace, &s.name),
                format!("LoadBalancer service {} has no external address", s.name),
                "No load balancer address has been assigned yet. On a local cluster this is expected without a load balancer implementation; in the cloud it usually means provisioning is slow or failing.".into(),
                vec!["status.loadBalancer.ingress: <empty>".into()],
                "Check the service's events and your cloud load balancer controller's logs.".into(),
                cmds,
                vec![],
            ));
        }
    }
}

/* ---------------- Service ports ---------------- */

fn service_port_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    for s in &g.services {
        let pods: Vec<&PodInfo> =
            g.pods.iter().filter(|p| p.namespace == s.namespace && s.pods.contains(&p.name)).collect();
        let Some(sample) = pods.first() else { continue };
        for sp in &s.ports_detail {
            let cmds = vec![
                format!("kubectl get svc {} -n {} -o yaml", s.name, s.namespace),
                format!(
                    "kubectl get pod {} -n {} -o jsonpath='{{.spec.containers[*].ports}}'",
                    sample.name, s.namespace
                ),
            ];
            let declared: Vec<String> = sample
                .container_ports
                .iter()
                .map(|c| match &c.name {
                    Some(n) => format!("{n}={}/{}", c.port, c.protocol),
                    None => format!("{}/{}", c.port, c.protocol),
                })
                .collect();
            if sp.target.parse::<i32>().is_err() {
                if !pods.iter().any(|p| p.container_ports.iter().any(|c| c.name.as_deref() == Some(sp.target.as_str())))
                {
                    out.push(issue(
                        format!("svc-port-name:{}/{}:{}", s.namespace, s.name, sp.port),
                        Severity::Critical,
                        Layer::Service,
                        target("Service", &s.namespace, &s.name),
                        format!("Service {} targets port name \"{}\", which no pod defines", s.name, sp.target),
                        "A named targetPort must match a named containerPort. Because none does, the service has no endpoints for this port.".into(),
                        vec![
                            format!("service port {} targetPort: {}", sp.port, sp.target),
                            format!("container ports on {}: {}", sample.name, if declared.is_empty() { "<none>".into() } else { declared.join(", ") }),
                        ],
                        "Name the container port to match, or change targetPort to the number the app listens on.".into(),
                        cmds,
                        vec![],
                    ));
                }
            } else if !sample.container_ports.is_empty() {
                let n: i32 = sp.target.parse().unwrap_or(0);
                if !sample.container_ports.iter().any(|c| c.port == n && c.protocol.eq_ignore_ascii_case(&sp.protocol))
                {
                    out.push(issue(
                        format!("svc-port-mismatch:{}/{}:{}", s.namespace, s.name, sp.port),
                        Severity::Warning,
                        Layer::Service,
                        target("Service", &s.namespace, &s.name),
                        format!("Service {} sends to port {}, but the pods declare {}", s.name, n, declared.join(", ")),
                        "Declared container ports are informational, so this may be fine. But if the app really listens only on the declared port, connections through the service are refused.".into(),
                        vec![format!("service port {} targetPort: {n}/{}", sp.port, sp.protocol), format!("container ports: {}", declared.join(", "))],
                        "Make targetPort match the port the application actually listens on.".into(),
                        cmds,
                        vec![],
                    ));
                }
            }
        }
    }
}

/* ---------------- Storage and config mounts ---------------- */

fn storage_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    for pvc in g.pvcs.iter().filter(|p| p.phase == "Pending") {
        let users: Vec<String> = g
            .pods
            .iter()
            .filter(|p| p.namespace == pvc.namespace && p.pvcs.contains(&pvc.name))
            .map(|p| p.name.clone())
            .collect();
        let evs: Vec<String> = g
            .events
            .iter()
            .filter(|e| e.kind == "PersistentVolumeClaim" && e.namespace == pvc.namespace && e.name == pvc.name)
            .take(3)
            .map(|e| format!("{}: {}", e.reason, e.message))
            .collect();
        let msg = evs.join(" ");
        out.push(issue(
            format!("storage-pvc-pending:{}/{}", pvc.namespace, pvc.name),
            if users.is_empty() { Severity::Warning } else { Severity::Critical },
            Layer::Pod,
            target("PersistentVolumeClaim", &pvc.namespace, &pvc.name),
            format!("Volume claim {} isn't bound", pvc.name),
            if users.is_empty() {
                "No pod uses it yet. With WaitForFirstConsumer storage classes this is normal until a pod is scheduled.".into()
            } else {
                format!("{} pods can't start until the claim gets a volume.", users.len())
            },
            std::iter::once(format!("storageClassName: {}", pvc.storage_class.clone().unwrap_or_else(|| "<default>".into())))
                .chain(evs)
                .collect(),
            if msg.contains("not found") && msg.contains("storageclass") {
                "The storage class doesn't exist. Use one from kubectl get storageclass.".into()
            } else if msg.contains("waiting for a volume to be created") || msg.contains("ExternalProvisioning") {
                "The CSI driver hasn't created the volume. Check the driver's controller pods (for EBS: ebs-csi-controller) and its IAM permissions.".into()
            } else {
                "Check the storage class exists and its provisioner is running.".into()
            },
            vec![format!("kubectl describe pvc {} -n {}", pvc.name, pvc.namespace), "kubectl get storageclass".into()],
            users,
        ));
    }

    // FailedMount / FailedAttachVolume on pods, split into missing config vs storage.
    let mut groups: BTreeMap<(String, bool), (Vec<&PodInfo>, String)> = BTreeMap::new();
    for e in
        g.events.iter().filter(|e| e.kind == "Pod" && (e.reason == "FailedMount" || e.reason == "FailedAttachVolume"))
    {
        let Some(p) = g.pods.iter().find(|p| p.namespace == e.namespace && p.name == e.name) else { continue };
        if p.phase == "Running" && p.ready {
            continue;
        }
        let m = e.message.to_lowercase();
        let config = (m.contains("configmap") || m.contains("secret")) && m.contains("not found");
        let owner = p.workload.clone().unwrap_or_else(|| workload_id("Pod", &p.namespace, &p.name));
        let entry = groups.entry((owner, config)).or_insert_with(|| (vec![], e.message.clone()));
        if !entry.0.iter().any(|x| x.name == p.name) {
            entry.0.push(p);
        }
    }
    for ((owner, config), (pods, msg)) in groups {
        let (k, ns, n) = split_id(&owner);
        let ns = if ns.is_empty() { pods[0].namespace.as_str() } else { ns };
        out.push(issue(
            format!("{}:{owner}", if config { "config-missing" } else { "storage-mount-failed" }),
            Severity::Critical,
            Layer::Pod,
            target(k, ns, n),
            if config {
                format!("{} mounts a ConfigMap or Secret that doesn't exist", owner_label(&owner))
            } else {
                format!("{} can't mount its volume", owner_label(&owner))
            },
            format!("{} pods are stuck in ContainerCreating until the volume mounts.", pods.len()),
            vec![msg.clone()],
            if config {
                "Create the missing ConfigMap or Secret in the same namespace, or fix the name in the pod spec.".into()
            } else if msg.contains("Multi-Attach") {
                "A ReadWriteOnce volume is still attached to another node, often from the old pod during a rollout. Wait for it to detach, or use a Recreate strategy for this workload.".into()
            } else {
                "Check the CSI driver's node pods on this node and the volume's state in your cloud console.".into()
            },
            vec![format!("kubectl describe pod {} -n {ns}", pods[0].name)],
            pods.iter().map(|p| p.name.clone()).collect(),
        ));
    }
}

/* ---------------- Admission and quota ---------------- */

fn admission_and_quota_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    let mut seen = BTreeSet::new();
    for e in g.events.iter().filter(|e| e.reason == "FailedCreate") {
        let owner = if e.kind == "ReplicaSet" {
            g.workloads
                .iter()
                .filter(|w| {
                    w.kind == "Deployment" && w.namespace == e.namespace && e.name.starts_with(&format!("{}-", w.name))
                })
                .max_by_key(|w| w.name.len())
                .map(|w| w.id.clone())
        } else {
            g.workloads
                .iter()
                .find(|w| w.kind == e.kind && w.namespace == e.namespace && w.name == e.name)
                .map(|w| w.id.clone())
        }
        .unwrap_or_else(|| workload_id(&e.kind, &e.namespace, &e.name));
        let m = e.message.to_lowercase();
        let (rule, title, suggestion) = if m.contains("exceeded quota") {
            (
                "quota-exceeded",
                format!("{} can't create pods: namespace quota exceeded", owner_label(&owner)),
                "Raise the ResourceQuota, lower this workload's requests, or free capacity in the namespace."
                    .to_string(),
            )
        } else if m.contains("admission webhook") || m.contains("violates podsecurity") || m.contains("forbidden") {
            (
                "admission-denied",
                format!("{} can't create pods: rejected by an admission policy", owner_label(&owner)),
                if m.contains("podsecurity") {
                    "The pod spec breaks the namespace's Pod Security level. Fix the securityContext fields named above, or change the namespace's pod-security label.".into()
                } else {
                    "A policy engine or webhook (for example Kyverno, Gatekeeper) rejected the pod. Fix the field it names, or ask for an exception.".into()
                },
            )
        } else {
            continue;
        };
        if !seen.insert((rule, owner.clone())) {
            continue;
        }
        let (k, _, n) = split_id(&owner);
        out.push(issue(
            format!("{rule}:{owner}"),
            Severity::Critical,
            Layer::Workload,
            target(k, &e.namespace, n),
            title,
            "The controller keeps trying to create pods, but the API server refuses every attempt, so the workload stays below its desired replicas.".into(),
            vec![format!("FailedCreate on {}/{}: {}", e.kind.to_lowercase(), e.name, e.message)],
            suggestion,
            vec![
                format!("kubectl describe {} {} -n {}", e.kind.to_lowercase(), e.name, e.namespace),
                format!("kubectl describe resourcequota -n {}", e.namespace),
            ],
            vec![],
        ));
    }
}

/* ---------------- Autoscaling ---------------- */

fn hpa_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    for h in &g.hpas {
        let cond = |t: &str| h.conditions.iter().find(|c| c.type_ == t);
        let t = target("HorizontalPodAutoscaler", &h.namespace, &h.name);
        let cmds = vec![format!("kubectl describe hpa {} -n {}", h.name, h.namespace)];
        let ev = |c: &ConditionInfo| {
            format!(
                "{} {}: {} {}",
                c.type_,
                c.status,
                c.reason.clone().unwrap_or_default(),
                c.message.clone().unwrap_or_default()
            )
        };
        if let Some(c) = cond("ScalingActive").filter(|c| c.status == "False") {
            let msg = c.message.clone().unwrap_or_default();
            out.push(issue(
                format!("hpa-metrics:{}/{}", h.namespace, h.name),
                Severity::Warning,
                Layer::Workload,
                t.clone(),
                format!("Autoscaler {} can't read metrics, so it isn't scaling", h.name),
                format!("{} stays at {} replicas regardless of load.", h.target, h.current),
                vec![ev(c)],
                if msg.contains("metrics.k8s.io") || msg.contains("unable to fetch metrics") || msg.contains("unable to get metrics") {
                    "Install or fix metrics-server, and make sure the target pods set CPU/memory requests (utilisation targets need them).".into()
                } else if msg.contains("missing request") {
                    "Set CPU or memory requests on every container in the target; utilisation targets need them.".into()
                } else {
                    "Check the metric source named above is available.".into()
                },
                cmds.clone(),
                vec![],
            ));
        } else if let Some(c) = cond("ScalingLimited").filter(|c| c.status == "True" && h.current >= h.max) {
            out.push(issue(
                format!("hpa-at-max:{}/{}", h.namespace, h.name),
                Severity::Warning,
                Layer::Workload,
                t.clone(),
                format!("Autoscaler {} is at its maximum of {} replicas", h.name, h.max),
                "It wants more capacity but can't add pods. If latency or errors are rising, this is a likely reason."
                    .into(),
                vec![ev(c), format!("current {}, desired {}, max {}", h.current, h.desired, h.max)],
                "Raise maxReplicas if the cluster has room, or look at why load per pod has grown.".into(),
                cmds.clone(),
                vec![],
            ));
        }
        if let Some(c) = cond("AbleToScale").filter(|c| c.status == "False") {
            out.push(issue(
                format!("hpa-unable:{}/{}", h.namespace, h.name),
                Severity::Warning,
                Layer::Workload,
                t,
                format!("Autoscaler {} can't change the replica count", h.name),
                format!("It can't update {}.", h.target),
                vec![ev(c)],
                "Check the scale target exists and the HPA controller has permission to scale it.".into(),
                cmds,
                vec![],
            ));
        }
    }
}

/* ---------------- Evictions ---------------- */

fn eviction_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    for (owner, pods) in group_by_owner(g.pods.iter().filter(|p| p.status == "Evicted")) {
        let (k, _, n) = split_id(&owner);
        let msg = pods[0].message.clone().unwrap_or_default();
        out.push(issue(
            format!("pod-evicted:{owner}"),
            Severity::Warning,
            Layer::Node,
            target(k, &pods[0].namespace, n),
            format!("{} had {} pods evicted", owner_label(&owner), pods.len()),
            "The kubelet evicted these pods to protect the node, usually because it ran low on memory or disk.".into(),
            if msg.is_empty() { vec![] } else { vec![format!("message: {msg}")] },
            if msg.contains("ephemeral-storage") {
                "Set ephemeral-storage requests and limits, and stop the app writing large files to its container filesystem.".into()
            } else if msg.contains("memory") {
                "Set memory requests that reflect real usage so the scheduler doesn't overpack nodes.".into()
            } else {
                "Check the node's pressure conditions around the eviction time.".into()
            },
            vec![format!("kubectl get pods -n {} --field-selector=status.phase=Failed", pods[0].namespace)],
            pods.iter().map(|p| p.name.clone()).collect(),
        ));
    }
}

/* ---------------- Service mesh (Istio) ---------------- */

fn has_sidecar(p: &PodInfo) -> bool {
    p.containers.iter().any(|c| c.name == "istio-proxy") || p.init_containers.iter().any(|c| c == "istio-proxy")
}

/// Resolve an Istio destination host to a (namespace, service) in this cluster.
fn resolve_host(host: &str, default_ns: &str) -> Option<(String, String)> {
    let host = host.trim_end_matches(".cluster.local").trim_end_matches(".svc");
    let parts: Vec<&str> = host.split('.').collect();
    match parts.as_slice() {
        [svc] => Some((default_ns.into(), (*svc).into())),
        [svc, ns] => Some(((*ns).into(), (*svc).into())),
        _ => None, // external host, handled by ServiceEntry
    }
}

fn mesh_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    let sidecars_seen = g.pods.iter().any(has_sidecar);
    if !g.mesh.installed && !sidecars_seen {
        return;
    }
    // Pods in injected namespaces that are missing the sidecar.
    let injected: BTreeSet<&str> = g
        .namespaces
        .iter()
        .filter(|n| {
            n.labels.get("istio-injection").map(String::as_str) == Some("enabled")
                || n.labels.contains_key("istio.io/rev")
        })
        .map(|n| n.name.as_str())
        .collect();
    let missing = g.pods.iter().filter(|p| {
        injected.contains(p.namespace.as_str())
            && p.phase == "Running"
            && !has_sidecar(p)
            && p.labels.get("sidecar.istio.io/inject").map(String::as_str) != Some("false")
    });
    for (owner, pods) in group_by_owner(missing) {
        let (k, _, n) = split_id(&owner);
        let ns = &pods[0].namespace;
        out.push(issue(
            format!("mesh-no-sidecar:{owner}"),
            Severity::Warning,
            Layer::Pod,
            target(k, ns, n),
            format!("{} runs without the Istio sidecar in an injected namespace", owner_label(&owner)),
            "These pods started before injection was enabled, or injection failed. With STRICT mTLS, calls to and from them are rejected, and mesh routing rules don't apply to them.".into(),
            vec![format!("namespace {ns} has sidecar injection enabled"), format!("{} pods without istio-proxy", pods.len())],
            "Restart the workload so new pods get the sidecar. If they still don't, check the injector webhook.".into(),
            vec![
                format!("kubectl rollout restart {}/{} -n {ns}", k.to_lowercase(), n),
                format!("kubectl get pod {} -n {ns} -o jsonpath='{{.spec.containers[*].name}}'", pods[0].name),
            ],
            pods.iter().map(|p| p.name.clone()).collect(),
        ));
    }
    // VirtualService destinations and subsets.
    for vs in &g.mesh.virtual_services {
        let t = target("VirtualService", &vs.namespace, &vs.name);
        let mut reported = BTreeSet::new();
        for (host, subset) in &vs.destinations {
            let Some((ns, svc_name)) = resolve_host(host, &vs.namespace) else { continue };
            if !reported.insert((host.clone(), subset.clone())) {
                continue;
            }
            let cmds = vec![format!("kubectl get virtualservice {} -n {} -o yaml", vs.name, vs.namespace)];
            let Some(svc) = g.services.iter().find(|s| s.namespace == ns && s.name == svc_name) else {
                out.push(issue(
                    format!("mesh-dest-missing:{}/{}/{host}", vs.namespace, vs.name),
                    Severity::Critical,
                    Layer::Entry,
                    t.clone(),
                    format!("VirtualService {} routes to {host}, which doesn't exist", vs.name),
                    "Requests matching this route get 503 (no healthy upstream) from the sidecar or gateway.".into(),
                    vec![format!("destination.host: {host}"), format!("looked for service {ns}/{svc_name}")],
                    "Fix the destination host, or create the service.".into(),
                    cmds,
                    vec![],
                ));
                continue;
            };
            let Some(sub) = subset else { continue };
            let dr = g
                .mesh
                .destination_rules
                .iter()
                .find(|d| resolve_host(&d.host, &d.namespace) == Some((ns.clone(), svc_name.clone())));
            let def = dr.and_then(|d| d.subsets.iter().find(|(n, _)| n == sub));
            match def {
                None => out.push(issue(
                    format!("mesh-subset-missing:{}/{}/{host}/{sub}", vs.namespace, vs.name),
                    Severity::Critical,
                    Layer::Entry,
                    t.clone(),
                    format!("VirtualService {} uses subset {sub}, which no DestinationRule defines", vs.name),
                    "Istio can't resolve the subset, so traffic to it fails with 503.".into(),
                    vec![
                        format!("destination: {host}, subset {sub}"),
                        match dr {
                            Some(d) => format!("DestinationRule {} defines: {}", d.name, d.subsets.iter().map(|s| s.0.clone()).collect::<Vec<_>>().join(", ")),
                            None => format!("no DestinationRule for {host}"),
                        },
                    ],
                    format!("Add subset {sub} to the DestinationRule for {host}, or change the route to an existing subset."),
                    cmds,
                    vec![],
                )),
                Some((_, labels)) => {
                    let mut sel = svc.selector.clone();
                    sel.extend(labels.clone());
                    let matching = g.pods.iter().filter(|p| p.namespace == ns && labels_match(&sel, &p.labels)).count();
                    if matching == 0 {
                        out.push(issue(
                            format!("mesh-subset-empty:{}/{}/{host}/{sub}", vs.namespace, vs.name),
                            Severity::Critical,
                            Layer::Entry,
                            t.clone(),
                            format!("Subset {sub} of {svc_name} matches no pods"),
                            "The route is valid but its subset selects nothing, so its share of traffic fails with 503. This often happens after a version label changes during a canary.".into(),
                            vec![format!("subset {sub} labels: {}", labels.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(","))],
                            "Point the subset at labels the current pods carry, or shift its weight to zero.".into(),
                            cmds,
                            vec![],
                        ));
                    }
                }
            }
        }
    }
}

/* ---------------- Cloud load balancer targets ---------------- */

fn lb_reason_advice(reason: &str, hc: &str, readiness_path: Option<&str>) -> String {
    match reason {
        "Target.ResponseCodeMismatch" => {
            let mut s = format!(
                "The health check reaches the pods but gets the wrong HTTP status ({hc}). Make that path return a success code, or point the health check at one that does (AWS Load Balancer Controller: the alb.ingress.kubernetes.io/healthcheck-path annotation)."
            );
            if let Some(p) = readiness_path {
                s.push_str(&format!(" Your readiness probe uses {p}, which is usually the right path."));
            }
            s
        }
        "Target.Timeout" => "The load balancer can't reach the targets in time. Most often a security group doesn't let the load balancer reach the node or pod port, or a NetworkPolicy doesn't admit the VPC CIDR.".into(),
        "Target.FailedHealthChecks" => format!("Health check connections fail ({hc}). Check the app listens on that port and security groups allow it."),
        "Target.NotInUse" => "Targets are in an Availability Zone the load balancer isn't enabled for. Add that zone's subnet to the load balancer.".into(),
        "Target.InvalidState" => "The target instances are stopped or terminated.".into(),
        "Target.IpUnusable" => "The target IP is in use by a load balancer or isn't valid; the pod may have been replaced.".into(),
        "Target.NotRegistered" => "Targets aren't registered. Check the TargetGroupBinding and the load balancer controller's logs.".into(),
        "Instance" | "ELB" => format!("The instances fail the health check ({hc})."),
        _ => format!("Check the health check settings ({hc}) against what the app serves."),
    }
}

fn lb_target_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    for h in g.lb_health.iter().filter(|h| h.error.is_none()) {
        let src = &h.source;
        for tg in &h.target_groups {
            let cmds = vec![format!(
                "aws elbv2 describe-target-health --target-group-arn $(aws elbv2 describe-target-groups --names {} --query 'TargetGroups[0].TargetGroupArn' --output text)",
                tg.name
            )];
            if tg.targets.is_empty() {
                out.push(issue(
                    format!("lb-no-targets:{}/{}/{}", src.namespace, src.name, tg.name),
                    Severity::Critical,
                    Layer::Entry,
                    src.clone(),
                    format!("Load balancer target group {} has no targets", tg.name),
                    format!("{} ({}) has nothing to send traffic to, so every request fails with 503.", h.lb_name, h.dns_name),
                    vec![format!("target group {}: 0 registered targets", tg.name), format!("health check: {}", tg.health_check)],
                    "Check the backing service has ready endpoints, and the load balancer controller's logs for registration errors.".into(),
                    cmds,
                    vec![],
                ));
                continue;
            }
            let bad: Vec<&TargetHealth> =
                tg.targets.iter().filter(|t| t.state == "unhealthy" || t.state == "unavailable").collect();
            let healthy = tg.targets.iter().filter(|t| t.state == "healthy").count();
            let stale: Vec<&TargetHealth> = tg
                .targets
                .iter()
                .filter(|t| t.resolved.is_none() && t.state != "draining" && tg.target_type == "ip")
                .collect();
            if !bad.is_empty() {
                let reason = bad.iter().filter_map(|t| t.reason.clone()).next().unwrap_or_default();
                let readiness_path = bad
                    .iter()
                    .filter_map(|t| t.resolved.as_ref())
                    .filter_map(|r| r.split_once('/'))
                    .filter_map(|(ns, name)| g.pods.iter().find(|p| p.namespace == ns && p.name == name))
                    .filter_map(|p| g.workloads.iter().find(|w| Some(&w.id) == p.workload.as_ref()))
                    .flat_map(|w| w.containers.iter().flat_map(|c| c.probes.iter()))
                    .find(|p| p.kind == "readiness" && p.handler == "http")
                    .and_then(|p| p.path.clone());
                out.push(issue(
                    format!("lb-targets-unhealthy:{}/{}/{}", src.namespace, src.name, tg.name),
                    if healthy == 0 { Severity::Critical } else { Severity::Warning },
                    Layer::Entry,
                    src.clone(),
                    format!("{} of {} load balancer targets are unhealthy in {}", bad.len(), tg.targets.len(), tg.name),
                    if healthy == 0 {
                        "The cloud load balancer considers every target unhealthy, so users get 502/503 even if the pods look fine inside the cluster.".into()
                    } else {
                        "Some targets fail the load balancer's health check and receive no traffic, reducing capacity.".into()
                    },
                    std::iter::once(format!("health check: {}", tg.health_check))
                        .chain(bad.iter().take(6).map(|t| {
                            format!(
                                "{}{}{}: {} {}{}",
                                t.id,
                                t.port.map(|p| format!(":{p}")).unwrap_or_default(),
                                t.resolved.as_ref().map(|r| format!(" ({r})")).unwrap_or_default(),
                                t.state,
                                t.reason.clone().unwrap_or_default(),
                                t.description.as_ref().map(|d| format!(": {d}")).unwrap_or_default()
                            )
                        }))
                        .collect(),
                    lb_reason_advice(&reason, &tg.health_check, readiness_path.as_deref()),
                    cmds.clone(),
                    bad.iter().filter_map(|t| t.resolved.as_ref()).filter_map(|r| r.split_once('/').map(|x| x.1.to_string())).collect(),
                ));
            }
            if !stale.is_empty() {
                out.push(issue(
                    format!("lb-stale-targets:{}/{}/{}", src.namespace, src.name, tg.name),
                    Severity::Warning,
                    Layer::Entry,
                    src.clone(),
                    format!("{} load balancer targets don't match any current pod", stale.len()),
                    "The target group still lists IPs that no pod uses. The controller may be lagging or failing to deregister them, and requests to those IPs fail.".into(),
                    stale.iter().take(6).map(|t| format!("{}: {}", t.id, t.state)).collect(),
                    "Check the AWS Load Balancer Controller logs for reconcile errors.".into(),
                    cmds,
                    vec![],
                ));
            }
        }
    }
    // GKE ingress-gce publishes backend health on the ingress itself.
    for ing in g.ingresses.iter().filter(|i| !i.gce_backends.is_empty()) {
        let bad: Vec<&(String, String)> =
            ing.gce_backends.iter().filter(|(_, h)| h.eq_ignore_ascii_case("UNHEALTHY")).collect();
        if bad.is_empty() {
            continue;
        }
        let all = bad.len() == ing.gce_backends.len();
        out.push(issue(
            format!("lb-gce-unhealthy:{}/{}", ing.namespace, ing.name),
            if all { Severity::Critical } else { Severity::Warning },
            Layer::Entry,
            target("Ingress", &ing.namespace, &ing.name),
            format!("{} of {} Google load balancer backends are unhealthy", bad.len(), ing.gce_backends.len()),
            "Google's health checks fail for these backends, so the load balancer won't send them traffic.".into(),
            bad.iter().map(|(b, h)| format!("{b}: {h}")).collect(),
            "GKE derives the health check from the pod's readiness probe, or uses / when there isn't one. Make sure that path returns 200, or set a BackendConfig healthCheck, and that firewall rules allow Google's health check ranges.".into(),
            vec![format!("kubectl describe ingress {} -n {}", ing.name, ing.namespace)],
            vec![],
        ));
    }
}

/* ---------------- Pod networking: CNI and kube-proxy ---------------- */

const CNI_AGENTS: [&str; 11] = [
    "aws-node",
    "calico-node",
    "cilium",
    "kube-flannel-ds",
    "kube-flannel",
    "weave-net",
    "canal",
    "antrea-agent",
    "kube-router",
    "ovnkube-node",
    "azure-cni",
];

fn cni_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    for w in g.workloads.iter().filter(|w| w.kind == "DaemonSet" && w.namespace == "kube-system") {
        let is_cni = CNI_AGENTS.contains(&w.name.as_str());
        let is_proxy = w.name == "kube-proxy";
        if !(is_cni || is_proxy) || w.ready >= w.desired {
            continue;
        }
        let bad_nodes: Vec<String> = g
            .pods
            .iter()
            .filter(|p| p.workload.as_ref() == Some(&w.id) && !p.ready)
            .filter_map(|p| p.node.clone())
            .collect();
        let affected = g
            .pods
            .iter()
            .filter(|p| p.node.as_ref().is_some_and(|n| bad_nodes.contains(n)) && p.namespace != "kube-system")
            .count();
        let (rule, what, effect) = if is_proxy {
            (
                "kubeproxy-unready",
                "kube-proxy",
                "Service IPs may stop working on those nodes: new or changed services aren't programmed, so pods there can't reach them.",
            )
        } else {
            ("cni-agent-unready", "the CNI agent", "Pods on those nodes can't get IP addresses or reach the network; new pods there get stuck in ContainerCreating.")
        };
        out.push(issue(
            format!("{rule}:{}", w.id),
            Severity::Critical,
            Layer::Node,
            target(&w.kind, &w.namespace, &w.name),
            format!("{} of {} {} pods aren't ready", w.desired - w.ready, w.desired, w.name),
            format!("{} isn't running properly on {}. {effect}", what, if bad_nodes.is_empty() { "some nodes".to_string() } else { bad_nodes.join(", ") }),
            vec![
                format!("{}: desired {}, ready {}", w.name, w.desired, w.ready),
                format!("other pods on affected nodes: {affected}"),
            ],
            if is_proxy {
                "Check the kube-proxy pod logs on the affected nodes.".into()
            } else if w.name == "aws-node" {
                "Check the aws-node logs and the node role's permissions for the VPC CNI (AmazonEKS_CNI_Policy), and that the add-on version matches the cluster.".into()
            } else {
                "Check the CNI agent's logs on the affected nodes.".into()
            },
            vec![format!("kubectl get pods -n kube-system -o wide | grep {}", w.name), format!("kubectl logs -n kube-system ds/{} --tail=50", w.name)],
            vec![],
        ));
    }
    for n in g.nodes.iter().filter(|n| n.network_unavailable) {
        out.push(issue(
            format!("cni-node-network:{}", n.name),
            Severity::Critical,
            Layer::Node,
            target("Node", "", &n.name),
            format!("Node {} reports NetworkUnavailable", n.name),
            "The network plugin hasn't configured routes for this node, so its pods can't talk to the rest of the cluster.".into(),
            vec!["condition NetworkUnavailable: True".into()],
            "Check the CNI agent pod on this node.".into(),
            vec![format!("kubectl describe node {}", n.name)],
            vec![],
        ));
    }
    // Sandbox failures from the CNI, grouped: IP exhaustion vs anything else.
    let mut ip_ex: BTreeMap<String, (Vec<&PodInfo>, String)> = BTreeMap::new();
    let mut other: BTreeMap<String, (Vec<&PodInfo>, String)> = BTreeMap::new();
    for e in g.events.iter().filter(|e| e.kind == "Pod" && e.reason == "FailedCreatePodSandBox") {
        let Some(p) = g.pods.iter().find(|p| p.namespace == e.namespace && p.name == e.name) else { continue };
        if p.phase != "Pending" {
            continue;
        }
        let m = e.message.to_lowercase();
        let node = p.node.clone().unwrap_or_else(|| "unknown".into());
        let bucket = if m.contains("assign an ip")
            || m.contains("no available ip")
            || m.contains("insufficientfreeaddresses")
            || m.contains("failed to allocate")
        {
            &mut ip_ex
        } else if m.contains("network") || m.contains("cni") {
            &mut other
        } else {
            continue;
        };
        let entry = bucket.entry(node).or_insert_with(|| (vec![], e.message.clone()));
        entry.0.push(p);
    }
    for (node, (pods, msg)) in ip_ex {
        out.push(issue(
            format!("cni-ip-exhausted:{node}"),
            Severity::Critical,
            Layer::Node,
            target("Node", "", &node),
            format!("Pods on {node} can't get an IP address"),
            format!("{} pods are stuck in ContainerCreating because the network plugin has no free IPs to give them.", pods.len()),
            vec![msg],
            "On EKS with the VPC CNI: enable prefix delegation (ENABLE_PREFIX_DELEGATION=true), add larger subnets or a secondary CIDR, or lower WARM_IP_TARGET. Also check the node's max-pods matches its ENI limits.".into(),
            vec![format!("kubectl describe node {node}"), "kubectl -n kube-system logs ds/aws-node --tail=50".into()],
            pods.iter().map(|p| p.name.clone()).collect(),
        ));
    }
    for (node, (pods, msg)) in other {
        out.push(issue(
            format!("cni-sandbox-failed:{node}"),
            Severity::Critical,
            Layer::Node,
            target("Node", "", &node),
            format!("Pods on {node} fail network setup"),
            format!("{} pods can't start because the network plugin failed to set up their sandbox.", pods.len()),
            vec![msg],
            "Check the CNI agent pod on this node and its logs; restarting it often clears stale state.".into(),
            vec![format!("kubectl get pods -n kube-system -o wide --field-selector spec.nodeName={node}")],
            pods.iter().map(|p| p.name.clone()).collect(),
        ));
    }
}

/* ---------------- Probe configuration ---------------- */

fn probe_rules(g: &ClusterGraph, out: &mut Vec<Issue>) {
    for w in &g.workloads {
        let pods: Vec<&PodInfo> = g.pods.iter().filter(|p| p.workload.as_ref() == Some(&w.id)).collect();
        let restarts: i32 = pods.iter().map(|p| p.restarts).sum();
        let cmds = vec![format!("kubectl get {} {} -n {} -o yaml", w.kind.to_lowercase(), w.name, w.namespace)];
        for c in &w.containers {
            // Named probe ports must exist on the container.
            for pr in &c.probes {
                let Some(port) = &pr.port else { continue };
                if port.parse::<i32>().is_err() && !c.ports.iter().any(|p| p.name.as_deref() == Some(port.as_str())) {
                    out.push(issue(
                        format!("probe-port-undefined:{}:{}:{}", w.id, c.name, pr.kind),
                        Severity::Critical,
                        Layer::Pod,
                        target(&w.kind, &w.namespace, &w.name),
                        format!(
                            "The {} probe on {} uses port \"{port}\", which the container doesn't define",
                            pr.kind, c.name
                        ),
                        "A named probe port must match a named containerPort, so this probe can never succeed.".into(),
                        vec![
                            format!("{} probe port: {port}", pr.kind),
                            format!(
                                "container ports: {}",
                                if c.ports.is_empty() {
                                    "<none>".to_string()
                                } else {
                                    c.ports
                                        .iter()
                                        .map(|p| format!("{}={}", p.name.clone().unwrap_or_default(), p.port))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                }
                            ),
                        ],
                        "Name the container port to match, or use the port number in the probe.".into(),
                        cmds.clone(),
                        vec![],
                    ));
                }
            }
            let live = c.probes.iter().find(|p| p.kind == "liveness");
            let ready = c.probes.iter().find(|p| p.kind == "readiness");
            let startup = c.probes.iter().find(|p| p.kind == "startup");
            // Probes timing out.
            let timeouts: Vec<&EventInfo> = g
                .events
                .iter()
                .filter(|e| {
                    e.reason == "Unhealthy"
                        && e.namespace == w.namespace
                        && pods.iter().any(|p| p.name == e.name)
                        && (e.message.contains("context deadline exceeded")
                            || e.message.contains("Client.Timeout")
                            || e.message.contains("timeout"))
                })
                .collect();
            if let Some(e) = timeouts.first() {
                let which = if e.message.starts_with("Liveness") {
                    live
                } else if e.message.starts_with("Startup") {
                    startup
                } else {
                    ready
                };
                if let Some(pr) = which {
                    out.push(issue(
                        format!("probe-timeout:{}:{}:{}", w.id, c.name, pr.kind),
                        Severity::Warning,
                        Layer::Pod,
                        target(&w.kind, &w.namespace, &w.name),
                        format!("The {} probe on {} is timing out after {}s", pr.kind, c.name, pr.timeout),
                        "The endpoint answers slower than the probe allows. Under load this marks pods unready or restarts them, which adds more load to the rest.".into(),
                        vec![format!("Unhealthy: {}", e.message), format!("timeoutSeconds: {}, periodSeconds: {}, failureThreshold: {}", pr.timeout, pr.period, pr.failure_threshold)],
                        "Make the endpoint cheap (no database or downstream calls in a liveness check), or raise timeoutSeconds.".into(),
                        cmds.clone(),
                        vec![],
                    ));
                }
            }
            // Liveness identical to readiness while the workload is restarting.
            if let (Some(l), Some(r)) = (live, ready) {
                let same = l.handler == r.handler && l.path == r.path && l.port == r.port;
                if same && restarts > 0 && l.handler != "exec" {
                    out.push(issue(
                        format!("probe-liveness-equals-readiness:{}:{}", w.id, c.name),
                        Severity::Warning,
                        Layer::Pod,
                        target(&w.kind, &w.namespace, &w.name),
                        format!("{} uses the same check for liveness and readiness, and is restarting", c.name),
                        format!("When the app is busy or a dependency is slow, the shared check fails and the kubelet restarts the pod instead of just taking it out of rotation. {restarts} restarts so far."),
                        vec![format!("liveness and readiness: {} {}{}", l.handler, l.path.clone().unwrap_or_default(), l.port.as_ref().map(|p| format!(" port {p}")).unwrap_or_default())],
                        "Keep readiness as is, and make liveness a lighter check that only fails if the process is truly stuck.".into(),
                        cmds.clone(),
                        vec![],
                    ));
                }
            }
            // Liveness killing slow starters.
            let liveness_kills = g.events.iter().any(|e| {
                e.reason == "Unhealthy"
                    && e.namespace == w.namespace
                    && pods.iter().any(|p| p.name == e.name)
                    && e.message.starts_with("Liveness")
            });
            if let (Some(l), None) = (live, startup) {
                if liveness_kills && restarts > 0 && l.initial_delay < 15 {
                    out.push(issue(
                        format!("probe-no-startup:{}:{}", w.id, c.name),
                        Severity::Warning,
                        Layer::Pod,
                        target(&w.kind, &w.namespace, &w.name),
                        format!("{} may be killed while it's still starting", c.name),
                        format!(
                            "Liveness checks begin after {}s and there's no startupProbe, so a slow start looks like a hang and the kubelet restarts it.",
                            l.initial_delay
                        ),
                        vec![format!("liveness: initialDelaySeconds {}, periodSeconds {}, failureThreshold {}", l.initial_delay, l.period, l.failure_threshold)],
                        "Add a startupProbe with the same check and a generous failureThreshold; liveness then only starts once the app is up.".into(),
                        cmds.clone(),
                        vec![],
                    ));
                }
            }
        }
    }
}
