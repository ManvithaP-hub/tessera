//! Reads the cluster (list calls only) and converts it into a [`ClusterGraph`].

use crate::diagnose::diagnose;
use crate::model::*;
use crate::quantity;
use crate::Result;
use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet};
use k8s_openapi::api::core::v1::{Container, ContainerStatus, Event, Node, Pod, PodSpec, Service};
use k8s_openapi::api::discovery::v1::EndpointSlice;
use k8s_openapi::api::networking::v1::{Ingress, IngressBackend};
use kube::api::{Api, ListParams, LogParams};
use kube::{Client, Resource};
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;

async fn list_all<K>(client: &Client, what: &str, lp: ListParams) -> std::result::Result<Vec<K>, String>
where
    K: Resource<DynamicType = ()> + Clone + DeserializeOwned + Debug,
{
    let api: Api<K> = Api::all(client.clone());
    api.list(&lp).await.map(|l| l.items).map_err(|e| describe_err(what, &e))
}

fn describe_err(what: &str, e: &kube::Error) -> String {
    let s = e.to_string();
    if s.contains("403") || s.to_lowercase().contains("forbidden") {
        format!("Your credentials can't list {what} across namespaces, so they're missing from the map.")
    } else if s.contains("404") || s.contains("the server could not find") {
        format!("This cluster doesn't serve {what}.")
    } else {
        format!("Couldn't list {what}: {s}")
    }
}

/// Take one read-only snapshot of the cluster and diagnose it.
pub async fn collect(client: Client, context: &str) -> Result<ClusterGraph> {
    // Fail fast with a clear error when the API server is unreachable or
    // credentials are rejected, instead of returning an empty map.
    let version = client.apiserver_version().await?;

    let lp = ListParams::default;
    let (nodes, pods, services, slices, ingresses, deps, sts, dss, rss, events) = tokio::join!(
        list_all::<Node>(&client, "nodes", lp()),
        list_all::<Pod>(&client, "pods", lp()),
        list_all::<Service>(&client, "services", lp()),
        list_all::<EndpointSlice>(&client, "endpoint slices", lp()),
        list_all::<Ingress>(&client, "ingresses", lp()),
        list_all::<Deployment>(&client, "deployments", lp()),
        list_all::<StatefulSet>(&client, "statefulsets", lp()),
        list_all::<DaemonSet>(&client, "daemonsets", lp()),
        list_all::<ReplicaSet>(&client, "replicasets", lp()),
        list_all::<Event>(&client, "events", lp().fields("type=Warning")),
    );

    let mut warnings = Vec::new();
    fn take<T>(r: std::result::Result<Vec<T>, String>, warnings: &mut Vec<String>) -> Vec<T> {
        r.unwrap_or_else(|w| {
            warnings.push(w);
            Vec::new()
        })
    }
    let raw = RawSnapshot {
        nodes: take(nodes, &mut warnings),
        pods: take(pods, &mut warnings),
        services: take(services, &mut warnings),
        slices: take(slices, &mut warnings),
        ingresses: take(ingresses, &mut warnings),
        deployments: take(deps, &mut warnings),
        statefulsets: take(sts, &mut warnings),
        daemonsets: take(dss, &mut warnings),
        replicasets: take(rss, &mut warnings),
        events: take(events, &mut warnings),
    };
    let mut g = build_graph(&raw, context);
    g.server_version = Some(version.git_version);
    g.warnings = warnings;
    Ok(g)
}

/// Raw API objects as listed from the cluster.
#[derive(Default)]
pub struct RawSnapshot {
    pub nodes: Vec<Node>,
    pub pods: Vec<Pod>,
    pub services: Vec<Service>,
    pub slices: Vec<EndpointSlice>,
    pub ingresses: Vec<Ingress>,
    pub deployments: Vec<Deployment>,
    pub statefulsets: Vec<StatefulSet>,
    pub daemonsets: Vec<DaemonSet>,
    pub replicasets: Vec<ReplicaSet>,
    pub events: Vec<Event>,
}

/// Convert raw objects into the graph and run diagnosis. Pure, so it can be
/// tested with fixtures.
pub fn build_graph(raw: &RawSnapshot, context: &str) -> ClusterGraph {
    let RawSnapshot {
        nodes,
        pods,
        services,
        slices,
        ingresses,
        deployments: deps,
        statefulsets: sts,
        daemonsets: dss,
        replicasets: rss,
        events,
    } = raw;
    let mut g = ClusterGraph { context: context.to_string(), fetched_at: now_unix(), ..Default::default() };

    g.nodes = nodes.iter().map(convert_node).collect();

    // Workloads
    for d in deps {
        let (ns, name) = ns_name(d);
        let spec = d.spec.as_ref();
        let st = d.status.as_ref();
        g.workloads.push(WorkloadInfo {
            id: workload_id("Deployment", &ns, &name),
            kind: "Deployment".into(),
            namespace: ns,
            name,
            desired: spec.and_then(|s| s.replicas).unwrap_or(1),
            ready: st.and_then(|s| s.ready_replicas).unwrap_or(0),
            available: st.and_then(|s| s.available_replicas).unwrap_or(0),
            pod_labels: spec
                .and_then(|s| s.template.metadata.as_ref())
                .and_then(|m| m.labels.clone())
                .unwrap_or_default(),
            containers: spec.and_then(|s| s.template.spec.as_ref()).map(container_specs).unwrap_or_default(),
        });
    }
    for s in sts {
        let (ns, name) = ns_name(s);
        let spec = s.spec.as_ref();
        let st = s.status.as_ref();
        g.workloads.push(WorkloadInfo {
            id: workload_id("StatefulSet", &ns, &name),
            kind: "StatefulSet".into(),
            namespace: ns,
            name,
            desired: spec.and_then(|s| s.replicas).unwrap_or(1),
            ready: st.and_then(|s| s.ready_replicas).unwrap_or(0),
            available: st.and_then(|s| s.available_replicas).unwrap_or(0),
            pod_labels: spec
                .and_then(|s| s.template.metadata.as_ref())
                .and_then(|m| m.labels.clone())
                .unwrap_or_default(),
            containers: spec.and_then(|s| s.template.spec.as_ref()).map(container_specs).unwrap_or_default(),
        });
    }
    for d in dss {
        let (ns, name) = ns_name(d);
        let spec = d.spec.as_ref();
        let st = d.status.as_ref();
        g.workloads.push(WorkloadInfo {
            id: workload_id("DaemonSet", &ns, &name),
            kind: "DaemonSet".into(),
            namespace: ns,
            name,
            desired: st.map(|s| s.desired_number_scheduled).unwrap_or(0),
            ready: st.map(|s| s.number_ready).unwrap_or(0),
            available: st.and_then(|s| s.number_available).unwrap_or(0),
            pod_labels: spec
                .and_then(|s| s.template.metadata.as_ref())
                .and_then(|m| m.labels.clone())
                .unwrap_or_default(),
            containers: spec.and_then(|s| s.template.spec.as_ref()).map(container_specs).unwrap_or_default(),
        });
    }

    // ReplicaSet -> Deployment, so pods can be traced to their deployment.
    let rs_owner: HashMap<(String, String), String> = rss
        .iter()
        .filter_map(|rs| {
            let (ns, name) = ns_name(rs);
            let owner = controller_of(rs.meta().owner_references.as_deref())?;
            (owner.0 == "Deployment").then(|| ((ns.clone(), name), workload_id("Deployment", &ns, &owner.1)))
        })
        .collect();

    g.pods = pods
        .iter()
        .map(|p| {
            let (ns, name) = ns_name(p);
            let workload =
                controller_of(p.metadata.owner_references.as_deref()).and_then(|(kind, oname)| match kind.as_str() {
                    "ReplicaSet" => rs_owner.get(&(ns.clone(), oname)).cloned(),
                    "StatefulSet" | "DaemonSet" => Some(workload_id(&kind, &ns, &oname)),
                    _ => None,
                });
            convert_pod(p, ns, name, workload)
        })
        .collect();

    // Endpoint counts per service from EndpointSlices.
    let mut eps: HashMap<(String, String), (u32, u32)> = HashMap::new();
    for s in slices {
        let (ns, _) = ns_name(s);
        let Some(svc) = s.metadata.labels.as_ref().and_then(|l| l.get("kubernetes.io/service-name")).cloned() else {
            continue;
        };
        let e = eps.entry((ns, svc)).or_default();
        for ep in &s.endpoints {
            // Per the API, a nil ready condition means ready.
            if ep.conditions.as_ref().and_then(|c| c.ready).unwrap_or(true) {
                e.0 += 1;
            } else {
                e.1 += 1;
            }
        }
    }

    for s in services {
        let (ns, name) = ns_name(s);
        let spec = s.spec.clone().unwrap_or_default();
        let selector: BTreeMap<String, String> = spec.selector.clone().unwrap_or_default();
        let matched: Vec<String> = g
            .pods
            .iter()
            .filter(|p| {
                p.namespace == ns && p.phase != "Succeeded" && p.phase != "Failed" && labels_match(&selector, &p.labels)
            })
            .map(|p| p.name.clone())
            .collect();
        let workloads: Vec<String> = g
            .workloads
            .iter()
            .filter(|w| w.namespace == ns && labels_match(&selector, &w.pod_labels))
            .map(|w| w.id.clone())
            .collect();
        let ports = spec
            .ports
            .unwrap_or_default()
            .iter()
            .map(|p| {
                let target = p.target_port.as_ref().map(|t| match t {
                    k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(i) => i.to_string(),
                    k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(s) => s.clone(),
                });
                match target {
                    Some(t) if t != p.port.to_string() => {
                        format!("{}→{}/{}", p.port, t, p.protocol.clone().unwrap_or_else(|| "TCP".into()))
                    }
                    _ => format!("{}/{}", p.port, p.protocol.clone().unwrap_or_else(|| "TCP".into())),
                }
            })
            .collect();
        let external = s
            .status
            .as_ref()
            .and_then(|st| st.load_balancer.as_ref())
            .and_then(|lb| lb.ingress.as_ref())
            .map(|v| v.iter().filter_map(|i| i.hostname.clone().or_else(|| i.ip.clone())).collect())
            .unwrap_or_default();
        let (ready, not_ready) = eps.get(&(ns.clone(), name.clone())).copied().unwrap_or((0, 0));
        g.services.push(ServiceInfo {
            namespace: ns,
            name,
            type_: spec.type_.unwrap_or_else(|| "ClusterIP".into()),
            cluster_ip: spec.cluster_ip,
            selector,
            ports,
            external,
            ready_endpoints: ready,
            not_ready_endpoints: not_ready,
            pods: matched,
            workloads,
        });
    }

    for ing in ingresses {
        let (ns, name) = ns_name(ing);
        let spec = ing.spec.clone().unwrap_or_default();
        let mut routes = Vec::new();
        if let Some(b) = spec.default_backend.as_ref() {
            if let Some((svc, port)) = backend(b) {
                routes.push(Route { host: None, path: "/*".into(), service: svc, port });
            }
        }
        for rule in spec.rules.unwrap_or_default() {
            for p in rule.http.map(|h| h.paths).unwrap_or_default() {
                if let Some((svc, port)) = backend(&p.backend) {
                    routes.push(Route {
                        host: rule.host.clone(),
                        path: p.path.unwrap_or_else(|| "/".into()),
                        service: svc,
                        port,
                    });
                }
            }
        }
        let addresses = ing
            .status
            .as_ref()
            .and_then(|s| s.load_balancer.as_ref())
            .and_then(|lb| lb.ingress.as_ref())
            .map(|v| v.iter().filter_map(|i| i.hostname.clone().or_else(|| i.ip.clone())).collect())
            .unwrap_or_default();
        g.ingresses.push(IngressInfo { namespace: ns, name, class_name: spec.ingress_class_name, addresses, routes });
    }

    g.events = events
        .iter()
        .map(|e| EventInfo {
            namespace: e.metadata.namespace.clone().unwrap_or_default(),
            kind: e.involved_object.kind.clone().unwrap_or_default(),
            name: e.involved_object.name.clone().unwrap_or_default(),
            type_: e.type_.clone().unwrap_or_default(),
            reason: e.reason.clone().unwrap_or_default(),
            message: e.message.clone().unwrap_or_default().trim().to_string(),
            count: e.count.unwrap_or(1),
            last_seen: e
                .last_timestamp
                .as_ref()
                .map(|t| t.0.to_string())
                .or_else(|| e.event_time.as_ref().map(|t| t.0.to_string()))
                .or_else(|| e.metadata.creation_timestamp.as_ref().map(|t| t.0.to_string())),
        })
        .collect();
    g.events.sort_by(|a, b| b.last_seen.cmp(&a.last_seen));

    g.issues = diagnose(&g);
    g
}

fn backend(b: &IngressBackend) -> Option<(String, Option<String>)> {
    let s = b.service.as_ref()?;
    let port = s.port.as_ref().and_then(|p| p.number.map(|n| n.to_string()).or_else(|| p.name.clone()));
    Some((s.name.clone(), port))
}

fn ns_name<K: Resource>(o: &K) -> (String, String) {
    let m = o.meta();
    (m.namespace.clone().unwrap_or_default(), m.name.clone().unwrap_or_default())
}

fn controller_of(
    refs: Option<&[k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference]>,
) -> Option<(String, String)> {
    refs?.iter().find(|r| r.controller == Some(true)).map(|r| (r.kind.clone(), r.name.clone()))
}

fn container_specs(spec: &PodSpec) -> Vec<ContainerSpecInfo> {
    spec.containers
        .iter()
        .map(|c| {
            let (cpu, mem_req, mem_lim) = resources(c);
            ContainerSpecInfo {
                name: c.name.clone(),
                image: c.image.clone().unwrap_or_default(),
                cpu_request_milli: cpu,
                memory_request_bytes: mem_req,
                memory_limit_bytes: mem_lim,
            }
        })
        .collect()
}

fn resources(c: &Container) -> (Option<i64>, Option<i64>, Option<i64>) {
    let r = c.resources.as_ref();
    let req = r.and_then(|r| r.requests.as_ref());
    let lim = r.and_then(|r| r.limits.as_ref());
    (
        req.and_then(|m| m.get("cpu")).and_then(|q| quantity::cpu_milli(&q.0)),
        req.and_then(|m| m.get("memory")).and_then(|q| quantity::bytes(&q.0)),
        lim.and_then(|m| m.get("memory")).and_then(|q| quantity::bytes(&q.0)),
    )
}

fn convert_node(n: &Node) -> NodeInfo {
    let conds = n.status.as_ref().and_then(|s| s.conditions.clone()).unwrap_or_default();
    let alloc = n.status.as_ref().and_then(|s| s.allocatable.clone()).unwrap_or_default();
    NodeInfo {
        name: n.metadata.name.clone().unwrap_or_default(),
        ready: conds.iter().any(|c| c.type_ == "Ready" && c.status == "True"),
        unschedulable: n.spec.as_ref().and_then(|s| s.unschedulable).unwrap_or(false),
        instance_type: n.metadata.labels.as_ref().and_then(|l| l.get("node.kubernetes.io/instance-type").cloned()),
        cpu_allocatable_milli: alloc.get("cpu").and_then(|q| quantity::cpu_milli(&q.0)).unwrap_or(0),
        memory_allocatable_bytes: alloc.get("memory").and_then(|q| quantity::bytes(&q.0)).unwrap_or(0),
        pressure: conds
            .iter()
            .filter(|c| c.type_ != "Ready" && c.status == "True" && c.type_.ends_with("Pressure"))
            .map(|c| c.type_.clone())
            .collect(),
    }
}

fn convert_pod(p: &Pod, namespace: String, name: String, workload: Option<String>) -> PodInfo {
    let status = p.status.clone().unwrap_or_default();
    let spec = p.spec.clone().unwrap_or_default();
    let phase = status.phase.clone().unwrap_or_else(|| "Unknown".into());
    let statuses: Vec<ContainerStatus> = status.container_statuses.clone().unwrap_or_default();
    let containers = spec
        .containers
        .iter()
        .map(|c| {
            let st = statuses.iter().find(|s| s.name == c.name);
            let (cpu, _, mem_lim) = resources(c);
            let state = st.and_then(|s| s.state.as_ref());
            let (state_name, reason, message) = match state {
                Some(s) if s.running.is_some() => ("running", None, None),
                Some(s) if s.waiting.is_some() => {
                    let w = s.waiting.as_ref().unwrap();
                    ("waiting", w.reason.clone(), w.message.clone())
                }
                Some(s) if s.terminated.is_some() => {
                    let t = s.terminated.as_ref().unwrap();
                    ("terminated", t.reason.clone(), t.message.clone())
                }
                _ => ("waiting", None, None),
            };
            let last = st.and_then(|s| s.last_state.as_ref()).and_then(|l| l.terminated.as_ref());
            ContainerStatusInfo {
                name: c.name.clone(),
                image: c.image.clone().unwrap_or_default(),
                ready: st.map(|s| s.ready).unwrap_or(false),
                restart_count: st.map(|s| s.restart_count).unwrap_or(0),
                state: state_name.into(),
                reason,
                message,
                last_reason: last.and_then(|t| t.reason.clone()),
                last_exit_code: last.map(|t| t.exit_code),
                cpu_request_milli: cpu,
                memory_limit_bytes: mem_lim,
            }
        })
        .collect::<Vec<_>>();
    let ready = status
        .conditions
        .as_ref()
        .map(|cs| cs.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
        .unwrap_or(false);
    PodInfo {
        status: display_status(p, &phase, &containers),
        restarts: containers.iter().map(|c| c.restart_count).sum(),
        phase,
        ready,
        node: spec.node_name.clone(),
        workload,
        labels: p.metadata.labels.clone().unwrap_or_default(),
        pod_ip: status.pod_ip.clone(),
        created: p.metadata.creation_timestamp.as_ref().map(|t| t.0.to_string()),
        containers,
        namespace,
        name,
    }
}

/// Approximates the STATUS column of `kubectl get pods`.
fn display_status(p: &Pod, phase: &str, containers: &[ContainerStatusInfo]) -> String {
    if p.metadata.deletion_timestamp.is_some() {
        return "Terminating".into();
    }
    let st = p.status.as_ref();
    if let Some(r) = st.and_then(|s| s.reason.clone()) {
        return r;
    }
    for ic in st.and_then(|s| s.init_container_statuses.clone()).unwrap_or_default() {
        if let Some(w) = ic.state.as_ref().and_then(|s| s.waiting.as_ref()).and_then(|w| w.reason.clone()) {
            if w != "PodInitializing" {
                return format!("Init:{w}");
            }
        }
    }
    for c in containers {
        if c.state != "running" {
            if let Some(r) = &c.reason {
                return r.clone();
            }
        }
    }
    phase.to_string()
}

fn now_unix() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    secs.to_string()
}

/// Read a pod's logs (tail only, with timestamps).
pub async fn pod_logs(
    client: Client,
    namespace: &str,
    name: &str,
    container: Option<String>,
    previous: bool,
    tail: i64,
) -> Result<String> {
    let api: Api<Pod> = Api::namespaced(client, namespace);
    let lp = LogParams { container, previous, tail_lines: Some(tail), timestamps: true, ..Default::default() };
    Ok(api.logs(name, &lp).await?)
}
