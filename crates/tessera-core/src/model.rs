//! Serializable snapshot of a cluster, shaped for the traffic map and the
//! diagnosis rules. Everything here is plain data so the rules can be unit
//! tested without a cluster.

use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClusterGraph {
    pub context: String,
    pub server_version: Option<String>,
    /// Unix seconds when the snapshot was taken.
    pub fetched_at: String,
    pub nodes: Vec<NodeInfo>,
    pub ingresses: Vec<IngressInfo>,
    pub services: Vec<ServiceInfo>,
    pub workloads: Vec<WorkloadInfo>,
    pub pods: Vec<PodInfo>,
    pub events: Vec<EventInfo>,
    pub namespaces: Vec<NamespaceInfo>,
    pub network_policies: Vec<NetworkPolicyInfo>,
    pub pvcs: Vec<PvcInfo>,
    pub hpas: Vec<HpaInfo>,
    /// Names of IngressClasses, and which one is the default.
    pub ingress_classes: Vec<String>,
    pub default_ingress_class: Option<String>,
    /// `namespace/name` of Secrets, when the user may list their metadata.
    /// `None` means we couldn't check, so secret-based rules stay quiet.
    pub secret_names: Option<Vec<String>>,
    pub mesh: MeshInfo,
    pub issues: Vec<Issue>,
    /// Non-fatal problems while collecting, such as RBAC denials.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    pub name: String,
    pub ready: bool,
    pub unschedulable: bool,
    pub instance_type: Option<String>,
    pub cpu_allocatable_milli: i64,
    pub memory_allocatable_bytes: i64,
    /// Conditions such as MemoryPressure that are currently True.
    pub pressure: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct IngressInfo {
    pub namespace: String,
    pub name: String,
    pub class_name: Option<String>,
    pub addresses: Vec<String>,
    pub routes: Vec<Route>,
    pub tls_secrets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Route {
    pub host: Option<String>,
    pub path: String,
    pub service: String,
    pub port: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInfo {
    pub namespace: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub cluster_ip: Option<String>,
    pub selector: BTreeMap<String, String>,
    pub ports: Vec<String>,
    pub ports_detail: Vec<ServicePortInfo>,
    pub external: Vec<String>,
    pub ready_endpoints: u32,
    pub not_ready_endpoints: u32,
    /// Names of pods in the same namespace that the selector matches.
    pub pods: Vec<String>,
    /// Ids of workloads whose pod template the selector matches.
    pub workloads: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadInfo {
    /// `Kind/namespace/name`
    pub id: String,
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub desired: i32,
    pub ready: i32,
    pub available: i32,
    pub pod_labels: BTreeMap<String, String>,
    pub containers: Vec<ContainerSpecInfo>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContainerSpecInfo {
    pub name: String,
    pub image: String,
    pub cpu_request_milli: Option<i64>,
    pub memory_request_bytes: Option<i64>,
    pub memory_limit_bytes: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PodInfo {
    pub namespace: String,
    pub name: String,
    /// What `kubectl get pods` would print in the STATUS column.
    pub status: String,
    pub phase: String,
    pub ready: bool,
    pub restarts: i32,
    pub node: Option<String>,
    /// Owning workload id, if any.
    pub workload: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub pod_ip: Option<String>,
    pub created: Option<String>,
    pub containers: Vec<ContainerStatusInfo>,
    /// Init containers, including native sidecars such as istio-proxy.
    pub init_containers: Vec<String>,
    pub container_ports: Vec<ContainerPortInfo>,
    pub pvcs: Vec<String>,
    /// Pod-level status message, e.g. why it was evicted.
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContainerPortInfo {
    pub name: Option<String>,
    pub port: i32,
    pub protocol: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServicePortInfo {
    pub port: i32,
    /// Number or port name, as written in the Service. Defaults to `port`.
    pub target: String,
    pub protocol: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NamespaceInfo {
    pub name: String,
    pub labels: BTreeMap<String, String>,
}

/// A NetworkPolicy reduced to what the rules need. Selectors that use
/// matchExpressions are marked `complex` and treated conservatively.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicyInfo {
    pub namespace: String,
    pub name: String,
    pub pod_selector: BTreeMap<String, String>,
    pub selector_complex: bool,
    pub ingress_type: bool,
    pub egress_type: bool,
    pub ingress: Vec<NpRule>,
    pub egress: Vec<NpRule>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NpRule {
    /// Empty means "all peers".
    pub peers: Vec<String>,
    /// Empty means "all ports".
    pub ports: Vec<NpPort>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NpPort {
    /// Number or name; `None` means every port of the protocol.
    pub port: Option<String>,
    pub end_port: Option<i32>,
    pub protocol: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PvcInfo {
    pub namespace: String,
    pub name: String,
    pub phase: String,
    pub storage_class: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HpaInfo {
    pub namespace: String,
    pub name: String,
    pub target: String,
    pub min: i32,
    pub max: i32,
    pub current: i32,
    pub desired: i32,
    pub conditions: Vec<ConditionInfo>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConditionInfo {
    #[serde(rename = "type")]
    pub type_: String,
    pub status: String,
    pub reason: Option<String>,
    pub message: Option<String>,
}

/// Istio routing objects, when Istio's CRDs are installed.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MeshInfo {
    pub installed: bool,
    pub virtual_services: Vec<VirtualServiceInfo>,
    pub destination_rules: Vec<DestinationRuleInfo>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VirtualServiceInfo {
    pub namespace: String,
    pub name: String,
    pub hosts: Vec<String>,
    /// (destination host, optional subset)
    pub destinations: Vec<(String, Option<String>)>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DestinationRuleInfo {
    pub namespace: String,
    pub name: String,
    pub host: String,
    pub subsets: Vec<(String, BTreeMap<String, String>)>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContainerStatusInfo {
    pub name: String,
    pub image: String,
    pub ready: bool,
    pub restart_count: i32,
    /// running, waiting or terminated
    pub state: String,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub last_reason: Option<String>,
    pub last_exit_code: Option<i32>,
    pub cpu_request_milli: Option<i64>,
    pub memory_limit_bytes: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EventInfo {
    pub namespace: String,
    pub kind: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub reason: String,
    pub message: String,
    pub count: i32,
    pub last_seen: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Warning,
}

/// Layers in the order a request travels through them.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    Entry,
    Service,
    Workload,
    Pod,
    Node,
}

/// What kind of problem an issue is, independent of where on the path it sits.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Routing,
    Network,
    Dns,
    Mesh,
    Image,
    Config,
    Storage,
    Scheduling,
    Capacity,
    Scaling,
    Runtime,
    Node,
    #[default]
    Other,
}

impl Category {
    /// Derived from the rule id prefix, so every rule is categorised in one place.
    pub fn for_rule(id: &str) -> Category {
        let rule = id.split(':').next().unwrap_or("");
        match rule {
            r if r.starts_with("entry-")
                || r.starts_with("ingress-")
                || r.starts_with("lb-")
                || r.starts_with("svc-") =>
            {
                Category::Routing
            }
            r if r.starts_with("np-") => Category::Network,
            r if r.starts_with("dns-") => Category::Dns,
            r if r.starts_with("mesh-") => Category::Mesh,
            "pod-imagepull" => Category::Image,
            "pod-config" | "config-missing" | "admission-denied" => Category::Config,
            r if r.starts_with("storage-") => Category::Storage,
            "pod-unschedulable" => Category::Scheduling,
            "quota-exceeded" => Category::Capacity,
            r if r.starts_with("hpa-") => Category::Scaling,
            "pod-oom" | "pod-crashloop" | "pod-notready" | "workload-unavailable" => Category::Runtime,
            r if r.starts_with("node-") || r == "pod-evicted" => Category::Node,
            _ => Category::Other,
        }
    }
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub kind: String,
    pub namespace: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub id: String,
    pub severity: Severity,
    pub layer: Layer,
    /// Filled in by `diagnose` from the rule id.
    pub category: Category,
    pub target: Target,
    pub title: String,
    pub detail: String,
    pub evidence: Vec<String>,
    pub suggestion: String,
    /// Read-only commands the user can run to confirm the finding.
    pub commands: Vec<String>,
    /// Pods this issue covers, when it is grouped per workload.
    pub pods: Vec<String>,
}

pub fn workload_id(kind: &str, namespace: &str, name: &str) -> String {
    format!("{kind}/{namespace}/{name}")
}

pub fn labels_match(selector: &BTreeMap<String, String>, labels: &BTreeMap<String, String>) -> bool {
    !selector.is_empty() && selector.iter().all(|(k, v)| labels.get(k) == Some(v))
}
