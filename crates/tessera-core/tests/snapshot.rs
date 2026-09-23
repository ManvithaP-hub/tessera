//! End-to-end conversion test: raw Kubernetes JSON in, diagnosed graph out.

use serde_json::json;
use tessera_core::{build_graph, Layer, RawSnapshot};

fn from<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> T {
    serde_json::from_value(v).expect("valid fixture")
}

fn fixture() -> RawSnapshot {
    let deploy = |name: &str, mem: &str| {
        from(json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": name, "namespace": "shop", "uid": format!("uid-{name}")},
            "spec": {
                "replicas": 2,
                "selector": {"matchLabels": {"app": name}},
                "template": {
                    "metadata": {"labels": {"app": name}},
                    "spec": {"containers": [{"name": name, "image": format!("{name}:1.0"),
                        "resources": {"requests": {"cpu": "250m", "memory": mem}, "limits": {"memory": mem}}}]}
                }
            },
            "status": {"replicas": 2, "readyReplicas": 0}
        }))
    };
    let rs = |name: &str| {
        from(json!({
            "apiVersion": "apps/v1", "kind": "ReplicaSet",
            "metadata": {"name": format!("{name}-7d9f"), "namespace": "shop",
                "ownerReferences": [{"apiVersion": "apps/v1", "kind": "Deployment", "name": name, "uid": format!("uid-{name}"), "controller": true}]},
            "spec": {"selector": {"matchLabels": {"app": name}}}
        }))
    };
    let pod = |name: &str, app: &str, status: serde_json::Value| {
        from(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": name, "namespace": "shop", "labels": {"app": app},
                "ownerReferences": [{"apiVersion": "apps/v1", "kind": "ReplicaSet", "name": format!("{app}-7d9f"), "uid": "x", "controller": true}]},
            "spec": {"nodeName": "node-a", "containers": [{"name": app, "image": format!("{app}:1.0"),
                "resources": {"requests": {"cpu": "250m", "memory": "256Mi"}, "limits": {"memory": "256Mi"}}}]},
            "status": status
        }))
    };
    let oom_status = json!({
        "phase": "Running",
        "conditions": [{"type": "Ready", "status": "False"}],
        "containerStatuses": [{"name": "payments", "image": "payments:1.0", "imageID": "", "ready": false, "restartCount": 7,
            "state": {"waiting": {"reason": "CrashLoopBackOff", "message": "back-off 5m0s"}},
            "lastState": {"terminated": {"reason": "OOMKilled", "exitCode": 137}}}]
    });
    let ok_status = json!({
        "phase": "Running", "podIP": "10.0.1.5",
        "conditions": [{"type": "Ready", "status": "True"}],
        "containerStatuses": [{"name": "catalog", "image": "catalog:1.0", "imageID": "", "ready": true, "restartCount": 0,
            "state": {"running": {}}}]
    });
    RawSnapshot {
        nodes: vec![from(json!({
            "apiVersion": "v1", "kind": "Node",
            "metadata": {"name": "node-a", "labels": {"node.kubernetes.io/instance-type": "m6i.xlarge"}},
            "status": {"allocatable": {"cpu": "3920m", "memory": "15564212Ki"},
                "conditions": [{"type": "Ready", "status": "True"}, {"type": "MemoryPressure", "status": "False"}]}
        }))],
        deployments: vec![deploy("payments", "256Mi"), deploy("catalog", "256Mi")],
        replicasets: vec![rs("payments"), rs("catalog")],
        pods: vec![
            pod("payments-7d9f-aaaaa", "payments", oom_status.clone()),
            pod("payments-7d9f-bbbbb", "payments", oom_status),
            pod("catalog-7d9f-ccccc", "catalog", ok_status),
        ],
        services: vec![
            from(json!({"apiVersion": "v1", "kind": "Service", "metadata": {"name": "payments", "namespace": "shop"},
                "spec": {"selector": {"app": "payments"}, "ports": [{"port": 80, "targetPort": 8080}]}})),
            from(json!({"apiVersion": "v1", "kind": "Service", "metadata": {"name": "catalog", "namespace": "shop"},
                "spec": {"selector": {"app": "catalog-svc"}, "ports": [{"port": 80, "targetPort": 8080}]}})),
        ],
        slices: vec![from(json!({
            "apiVersion": "discovery.k8s.io/v1", "kind": "EndpointSlice", "addressType": "IPv4",
            "metadata": {"name": "payments-abc", "namespace": "shop", "labels": {"kubernetes.io/service-name": "payments"}},
            "endpoints": [{"addresses": ["10.0.1.9"], "conditions": {"ready": false}}, {"addresses": ["10.0.1.10"], "conditions": {"ready": false}}]
        }))],
        ingresses: vec![from(json!({
            "apiVersion": "networking.k8s.io/v1", "kind": "Ingress",
            "metadata": {"name": "edge", "namespace": "shop"},
            "spec": {"ingressClassName": "alb", "rules": [{"host": "shop.example.com", "http": {"paths": [
                {"path": "/api/pay", "pathType": "Prefix", "backend": {"service": {"name": "payments", "port": {"number": 80}}}},
                {"path": "/api/catalog", "pathType": "Prefix", "backend": {"service": {"name": "catalog", "port": {"number": 80}}}},
                {"path": "/api/cart", "pathType": "Prefix", "backend": {"service": {"name": "cart", "port": {"number": 80}}}}
            ]}}]},
            "status": {"loadBalancer": {"ingress": [{"hostname": "k8s-edge-123.elb.amazonaws.com"}]}}
        }))],
        ..Default::default()
    }
}

#[test]
fn builds_and_diagnoses_a_realistic_snapshot() {
    let g = build_graph(&fixture(), "test");

    // Ownership: pods resolve through their ReplicaSet to the Deployment.
    assert!(g.pods.iter().all(|p| p.workload.as_deref().unwrap_or("").starts_with("Deployment/shop/")));
    let oom_pod = g.pods.iter().find(|p| p.name == "payments-7d9f-aaaaa").unwrap();
    assert_eq!(oom_pod.status, "CrashLoopBackOff");
    assert_eq!(oom_pod.containers[0].memory_limit_bytes, Some(256 << 20));

    // Service wiring and endpoint counts.
    let pay = g.services.iter().find(|s| s.name == "payments").unwrap();
    assert_eq!(pay.pods.len(), 2);
    assert_eq!((pay.ready_endpoints, pay.not_ready_endpoints), (0, 2));
    assert_eq!(pay.ports, vec!["80→8080/TCP"]);
    let cat = g.services.iter().find(|s| s.name == "catalog").unwrap();
    assert!(cat.pods.is_empty());

    // Ingress routes and address.
    assert_eq!(g.ingresses[0].routes.len(), 3);
    assert_eq!(g.ingresses[0].addresses, vec!["k8s-edge-123.elb.amazonaws.com"]);
    assert_eq!(g.nodes[0].cpu_allocatable_milli, 3920);

    // One issue per broken layer.
    let ids: Vec<&str> = g.issues.iter().map(|i| i.id.as_str()).collect();
    assert!(ids.contains(&"pod-oom:Deployment/shop/payments"), "{ids:?}");
    assert!(ids.contains(&"svc-no-match:shop/catalog"), "{ids:?}");
    assert!(ids.contains(&"entry-missing-backend:shop/edge/cart"), "{ids:?}");
    let entry = g.issues.iter().find(|i| i.layer == Layer::Entry).unwrap();
    assert!(entry.detail.contains("shop.example.com/api/cart"));
    let cat_issue = g.issues.iter().find(|i| i.id == "svc-no-match:shop/catalog").unwrap();
    assert!(cat_issue.suggestion.contains("app=catalog."), "{}", cat_issue.suggestion);

    // The graph serializes with camelCase keys for the frontend.
    let v = serde_json::to_value(&g).unwrap();
    assert!(v["services"][0].get("readyEndpoints").is_some());
    assert_eq!(v["issues"][0]["severity"], "critical");
}
