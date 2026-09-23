//! Tests for the extended rule set. Most build a ClusterGraph directly; the
//! NetworkPolicy test goes through raw JSON to also cover conversion.

use serde_json::json;
use std::collections::BTreeMap;
use tessera_core::*;

fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn pod(ns: &str, name: &str, app: &str) -> PodInfo {
    PodInfo {
        namespace: ns.into(),
        name: name.into(),
        status: "Running".into(),
        phase: "Running".into(),
        ready: true,
        node: Some("node-a".into()),
        workload: Some(workload_id("Deployment", ns, app)),
        labels: labels(&[("app", app)]),
        containers: vec![ContainerStatusInfo {
            name: app.into(),
            ready: true,
            state: "running".into(),
            ..Default::default()
        }],
        container_ports: vec![ContainerPortInfo { name: Some("http".into()), port: 8080, protocol: "TCP".into() }],
        ..Default::default()
    }
}

fn svc(ns: &str, name: &str, app: &str, pods: &[&str], target: &str) -> ServiceInfo {
    ServiceInfo {
        namespace: ns.into(),
        name: name.into(),
        type_: "ClusterIP".into(),
        selector: labels(&[("app", app)]),
        ports_detail: vec![ServicePortInfo { port: 80, target: target.into(), protocol: "TCP".into() }],
        ready_endpoints: pods.len() as u32,
        pods: pods.iter().map(|s| s.to_string()).collect(),
        workloads: vec![workload_id("Deployment", ns, app)],
        ..Default::default()
    }
}

fn ids(g: &ClusterGraph) -> Vec<String> {
    diagnose(g).into_iter().map(|i| i.id).collect()
}

fn find(g: &ClusterGraph, prefix: &str) -> Issue {
    diagnose(g).into_iter().find(|i| i.id.starts_with(prefix)).unwrap_or_else(|| panic!("no {prefix} in {:?}", ids(g)))
}

fn from<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> T {
    serde_json::from_value(v).expect("valid fixture")
}

#[test]
fn network_policies_from_raw_json() {
    let raw = RawSnapshot {
        pods: vec![from(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "api-1", "namespace": "shop", "labels": {"app": "api"}},
            "spec": {"nodeName": "n", "containers": [{"name": "api", "ports": [{"name": "http", "containerPort": 8080}]}]},
            "status": {"phase": "Running", "conditions": [{"type": "Ready", "status": "True"}],
                "containerStatuses": [{"name": "api", "image": "api", "imageID": "", "ready": true, "restartCount": 0, "state": {"running": {}}}]}
        }))],
        services: vec![from(
            json!({"apiVersion": "v1", "kind": "Service", "metadata": {"name": "api", "namespace": "shop"},
            "spec": {"selector": {"app": "api"}, "ports": [{"port": 80, "targetPort": "http"}]}}),
        )],
        network_policies: vec![
            // Only allows 9090, so 8080 (named "http") is blocked. No policyTypes: Ingress applies.
            from(json!({"apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
                "metadata": {"name": "metrics-only", "namespace": "shop"},
                "spec": {"podSelector": {"matchLabels": {"app": "api"}},
                    "ingress": [{"ports": [{"port": 9090}]}]}})),
            // Egress lockdown that forgot DNS.
            from(json!({"apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
                "metadata": {"name": "egress-db-only", "namespace": "shop"},
                "spec": {"podSelector": {}, "policyTypes": ["Egress"],
                    "egress": [{"to": [{"podSelector": {"matchLabels": {"app": "postgres"}}}], "ports": [{"port": 5432}]}]}})),
        ],
        ..Default::default()
    };
    let g = build_graph(&raw, "t");
    let blocked = g.issues.iter().find(|i| i.id.starts_with("np-ingress-blocked")).expect("ingress block");
    assert_eq!(blocked.severity, Severity::Critical);
    assert_eq!(blocked.category, Category::Network);
    assert!(blocked.evidence.iter().any(|e| e.contains("metrics-only")));
    let dns = g.issues.iter().find(|i| i.id.starts_with("np-egress-dns")).expect("dns block");
    assert!(dns.title.contains("DNS"));
    assert!(
        dns.evidence.iter().any(|e| e.contains("pods app=postgres in this namespace on 5432/TCP")),
        "{:?}",
        dns.evidence
    );
}

#[test]
fn network_policy_allowing_port_is_quiet() {
    let g = ClusterGraph {
        pods: vec![pod("shop", "api-1", "api")],
        services: vec![svc("shop", "api", "api", &["api-1"], "8080")],
        network_policies: vec![NetworkPolicyInfo {
            namespace: "shop".into(),
            name: "allow-http".into(),
            pod_selector: labels(&[("app", "api")]),
            ingress_type: true,
            egress_type: true,
            ingress: vec![NpRule {
                peers: vec![],
                ports: vec![NpPort { port: Some("8080".into()), end_port: None, protocol: "TCP".into() }],
            }],
            egress: vec![NpRule {
                peers: vec![],
                ports: vec![NpPort { port: Some("53".into()), end_port: None, protocol: "UDP".into() }],
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(ids(&g).iter().all(|i| !i.starts_with("np-")), "{:?}", ids(&g));
}

#[test]
fn cluster_dns_down() {
    let mut dns = svc("kube-system", "kube-dns", "kube-dns", &["coredns-1"], "53");
    dns.ready_endpoints = 0;
    dns.not_ready_endpoints = 2;
    let g = ClusterGraph { services: vec![dns], ..Default::default() };
    let i = find(&g, "dns-down");
    assert_eq!(i.category, Category::Dns);
    assert_eq!(i.severity, Severity::Critical);
}

#[test]
fn ingress_class_and_tls() {
    let g = ClusterGraph {
        ingress_classes: vec!["alb".into()],
        secret_names: Some(vec!["web/other-cert".into()]),
        services: vec![svc("web", "shop", "shop", &[], "8080")],
        ingresses: vec![IngressInfo {
            namespace: "web".into(),
            name: "edge".into(),
            class_name: Some("nginx".into()),
            routes: vec![Route { host: None, path: "/".into(), service: "shop".into(), port: None }],
            tls_secrets: vec!["shop-cert".into()],
            ..Default::default()
        }],
        ..Default::default()
    };
    let c = find(&g, "ingress-class-missing");
    assert!(c.suggestion.contains("alb"));
    find(&g, "ingress-tls-missing");
    // No duplicate "no address" when the class is already the problem.
    assert!(!ids(&g).iter().any(|i| i.starts_with("ingress-no-address")));
}

#[test]
fn load_balancer_failure_names_subnets() {
    let mut s = svc("web", "public", "shop", &[], "8080");
    s.type_ = "LoadBalancer".into();
    let g = ClusterGraph {
        services: vec![s],
        events: vec![EventInfo {
            namespace: "web".into(),
            kind: "Service".into(),
            name: "public".into(),
            type_: "Warning".into(),
            reason: "SyncLoadBalancerFailed".into(),
            message: "could not find any suitable subnets for creating the ELB".into(),
            count: 4,
            last_seen: None,
        }],
        ..Default::default()
    };
    let i = find(&g, "lb-failed");
    assert!(i.suggestion.contains("subnets"));
    assert_eq!(i.category, Category::Routing);
}

#[test]
fn named_target_port_missing() {
    let g = ClusterGraph {
        pods: vec![pod("web", "shop-1", "shop")],
        services: vec![svc("web", "shop", "shop", &["shop-1"], "web")],
        ..Default::default()
    };
    let i = find(&g, "svc-port-name");
    assert_eq!(i.severity, Severity::Critical);
    assert!(i.evidence.iter().any(|e| e.contains("http=8080/TCP")));
}

#[test]
fn storage_and_config_mounts() {
    let mut p = pod("db", "pg-0", "pg");
    p.phase = "Pending".into();
    p.ready = false;
    p.pvcs = vec!["data-pg-0".into()];
    let mut q = pod("web", "shop-1", "shop");
    q.phase = "Pending".into();
    q.ready = false;
    let g = ClusterGraph {
        pods: vec![p, q],
        pvcs: vec![PvcInfo {
            namespace: "db".into(),
            name: "data-pg-0".into(),
            phase: "Pending".into(),
            storage_class: Some("gp3".into()),
        }],
        events: vec![EventInfo {
            namespace: "web".into(),
            kind: "Pod".into(),
            name: "shop-1".into(),
            type_: "Warning".into(),
            reason: "FailedMount".into(),
            message: "MountVolume.SetUp failed for volume \"cfg\" : configmap \"shop-config\" not found".into(),
            count: 9,
            last_seen: None,
        }],
        ..Default::default()
    };
    let pvc = find(&g, "storage-pvc-pending");
    assert_eq!(pvc.severity, Severity::Critical);
    assert_eq!(pvc.category, Category::Storage);
    let cfg = find(&g, "config-missing");
    assert_eq!(cfg.category, Category::Config);
}

#[test]
fn quota_and_admission() {
    let w = |name: &str| WorkloadInfo {
        id: workload_id("Deployment", "team", name),
        kind: "Deployment".into(),
        namespace: "team".into(),
        name: name.into(),
        desired: 2,
        ..Default::default()
    };
    let ev = |rs: &str, msg: &str| EventInfo {
        namespace: "team".into(),
        kind: "ReplicaSet".into(),
        name: rs.into(),
        type_: "Warning".into(),
        reason: "FailedCreate".into(),
        message: msg.into(),
        count: 3,
        last_seen: None,
    };
    let g = ClusterGraph {
        workloads: vec![w("api"), w("api-worker")],
        events: vec![
            ev("api-5d8f7", "pods \"api-5d8f7-x\" is forbidden: exceeded quota: team-quota, requested: cpu=500m"),
            ev("api-worker-7c6", "admission webhook \"validate.kyverno.svc\" denied the request: require-labels"),
        ],
        ..Default::default()
    };
    let q = find(&g, "quota-exceeded");
    assert_eq!(q.target.name, "api");
    assert_eq!(q.category, Category::Capacity);
    let a = find(&g, "admission-denied");
    assert_eq!(a.target.name, "api-worker", "longest matching deployment name wins");
}

#[test]
fn hpa_without_metrics() {
    let g = ClusterGraph {
        hpas: vec![HpaInfo {
            namespace: "web".into(),
            name: "shop".into(),
            target: "Deployment/shop".into(),
            min: 2,
            max: 10,
            current: 2,
            desired: 2,
            conditions: vec![ConditionInfo {
                type_: "ScalingActive".into(),
                status: "False".into(),
                reason: Some("FailedGetResourceMetric".into()),
                message: Some(
                    "unable to get metrics for resource cpu: unable to fetch metrics from resource metrics API".into(),
                ),
            }],
        }],
        ..Default::default()
    };
    let i = find(&g, "hpa-metrics");
    assert!(i.suggestion.contains("metrics-server"));
    assert_eq!(i.category, Category::Scaling);
}

#[test]
fn liveness_probe_crashloop() {
    let mut p = pod("web", "shop-1", "shop");
    p.ready = false;
    p.status = "CrashLoopBackOff".into();
    p.containers[0].state = "waiting".into();
    p.containers[0].reason = Some("CrashLoopBackOff".into());
    p.containers[0].last_reason = Some("Error".into());
    p.containers[0].last_exit_code = Some(137);
    let g = ClusterGraph {
        pods: vec![p],
        events: vec![EventInfo {
            namespace: "web".into(),
            kind: "Pod".into(),
            name: "shop-1".into(),
            type_: "Warning".into(),
            reason: "Unhealthy".into(),
            message: "Liveness probe failed: HTTP probe failed with statuscode: 404".into(),
            count: 30,
            last_seen: None,
        }],
        ..Default::default()
    };
    let i = find(&g, "pod-crashloop");
    assert!(i.detail.contains("liveness probe"));
    assert!(i.evidence.iter().any(|e| e.contains("statuscode: 404")));
}

#[test]
fn istio_sidecar_and_subsets() {
    let mut v2 = pod("shop", "reviews-v1-1", "reviews");
    v2.labels.insert("version".into(), "v1".into());
    let mut proxied = pod("shop", "ratings-1", "ratings");
    proxied.containers.push(ContainerStatusInfo { name: "istio-proxy".into(), ..Default::default() });
    let g = ClusterGraph {
        namespaces: vec![NamespaceInfo { name: "shop".into(), labels: labels(&[("istio-injection", "enabled")]) }],
        pods: vec![v2, proxied],
        services: vec![svc("shop", "reviews", "reviews", &["reviews-v1-1"], "8080")],
        mesh: MeshInfo {
            installed: true,
            virtual_services: vec![VirtualServiceInfo {
                namespace: "shop".into(),
                name: "reviews".into(),
                hosts: vec!["reviews".into()],
                destinations: vec![
                    ("reviews".into(), Some("v1".into())),
                    ("reviews".into(), Some("v2".into())),
                    ("reviews".into(), Some("v3".into())),
                    ("details.shop.svc.cluster.local".into(), None),
                ],
            }],
            destination_rules: vec![DestinationRuleInfo {
                namespace: "shop".into(),
                name: "reviews".into(),
                host: "reviews".into(),
                subsets: vec![("v1".into(), labels(&[("version", "v1")])), ("v2".into(), labels(&[("version", "v2")]))],
            }],
        },
        ..Default::default()
    };
    let all = ids(&g);
    assert!(all.iter().any(|i| i.starts_with("mesh-no-sidecar:Deployment/shop/reviews")), "{all:?}");
    assert!(!all.iter().any(|i| i.starts_with("mesh-no-sidecar:Deployment/shop/ratings")), "{all:?}");
    assert!(all.iter().any(|i| i.ends_with("/reviews/v2") && i.starts_with("mesh-subset-empty")), "{all:?}");
    assert!(all.iter().any(|i| i.ends_with("/reviews/v3") && i.starts_with("mesh-subset-missing")), "{all:?}");
    assert!(all.iter().any(|i| i.starts_with("mesh-dest-missing") && i.contains("details")), "{all:?}");
    assert!(!all.iter().any(|i| i.ends_with("/reviews/v1")), "{all:?}");
    assert!(diagnose(&g).iter().filter(|i| i.id.starts_with("mesh-")).all(|i| i.category == Category::Mesh));
}

#[test]
fn every_rule_gets_a_category() {
    for id in [
        "np-egress-dns:x",
        "dns-down:x",
        "ingress-tls-missing:x",
        "lb-pending:x",
        "svc-port-name:x",
        "storage-mount-failed:x",
        "config-missing:x",
        "admission-denied:x",
        "quota-exceeded:x",
        "hpa-at-max:x",
        "pod-evicted:x",
        "mesh-subset-empty:x",
        "pod-oom:x",
        "pod-imagepull:x",
        "pod-unschedulable:x",
        "node-notready:x",
        "entry-missing-backend:x",
    ] {
        assert_ne!(Category::for_rule(id), Category::Other, "{id}");
    }
}
