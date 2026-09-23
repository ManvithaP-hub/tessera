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
