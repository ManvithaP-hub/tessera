//! Cloud load balancer target health.
//!
//! Tessera doesn't embed cloud SDKs or store cloud credentials. It calls the
//! same CLI your kubeconfig already uses to authenticate (for EKS, `aws`),
//! with a fixed list of read-only `describe` commands. Only runs when the
//! user turns on cloud checks.
//!
//! - AWS: ALB/NLB (elbv2) and Classic ELB, through the `aws` CLI.
//! - GKE: ingress-gce writes backend health into an ingress annotation, which
//!   is read passively in `collect` and needs no cloud call.
//! - Azure: not yet supported (see docs/architecture.md).

use crate::collect::CollectOptions;
use crate::model::*;
use kube::config::Kubeconfig;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct AwsEnv {
    pub profile: Option<String>,
    pub region: Option<String>,
}

impl AwsEnv {
    /// Use explicit settings if given, otherwise read `--profile`/`--region`
    /// and AWS_* env vars from the context's exec credential plugin, so the
    /// same identity that reaches the cluster is used.
    pub fn for_context(context: &str, opts: &CollectOptions) -> Self {
        let mut env = AwsEnv::default();
        if let Ok(kc) = Kubeconfig::read() {
            let user = kc
                .contexts
                .iter()
                .find(|c| c.name == context)
                .and_then(|c| c.context.as_ref())
                .and_then(|c| c.user.clone());
            let exec = user
                .and_then(|u| kc.auth_infos.iter().find(|a| a.name == u))
                .and_then(|a| a.auth_info.as_ref())
                .and_then(|a| a.exec.as_ref());
            if let Some(exec) = exec {
                let args = exec.args.clone().unwrap_or_default();
                let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
                env.profile = flag("--profile");
                env.region = flag("--region");
                for kv in exec.env.clone().unwrap_or_default() {
                    let (Some(k), Some(v)) = (kv.get("name"), kv.get("value")) else { continue };
                    match k.as_str() {
                        "AWS_PROFILE" => env.profile = Some(v.clone()),
                        "AWS_REGION" | "AWS_DEFAULT_REGION" => env.region = env.region.clone().or(Some(v.clone())),
                        _ => {}
                    }
                }
            }
        }
        if opts.aws_profile.as_deref().is_some_and(|p| !p.is_empty()) {
            env.profile = opts.aws_profile.clone();
        }
        if opts.aws_region.as_deref().is_some_and(|r| !r.is_empty()) {
            env.region = opts.aws_region.clone();
        }
        env
    }
}

/// The only AWS commands Tessera runs. All are read-only Describe calls.
const AWS_ALLOWED: [&str; 5] = [
    "elbv2 describe-load-balancers",
    "elbv2 describe-target-groups",
    "elbv2 describe-target-health",
    "elb describe-load-balancers",
    "elb describe-instance-health",
];

async fn aws(env: &AwsEnv, region: &str, args: &[&str]) -> Result<Value, String> {
    let verb = format!("{} {}", args[0], args[1]);
    assert!(AWS_ALLOWED.contains(&verb.as_str()), "non read-only AWS call: {verb}");
    let mut cmd = tokio::process::Command::new("aws");
    cmd.args(args).args(["--region", region, "--output", "json"]).env("AWS_PAGER", "").kill_on_drop(true);
    if let Some(p) = &env.profile {
        cmd.env("AWS_PROFILE", p);
    }
    let out = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| "The aws CLI didn't answer within 30 seconds.".to_string())?
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "Cloud checks need the aws CLI, which isn't installed or isn't on PATH.".to_string()
            } else {
                format!("Couldn't run the aws CLI: {e}")
            }
        })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(explain_aws_error(&err));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("Unexpected aws CLI output: {e}"))
}

pub fn explain_aws_error(stderr: &str) -> String {
    let e = stderr.trim();
    if e.contains("ExpiredToken") || e.contains("expired") || e.contains("Token has expired") {
        "Your AWS session has expired. Run `aws sso login` (or refresh your credentials), then refresh Tessera.".into()
    } else if e.contains("AccessDenied") || e.contains("UnauthorizedOperation") || e.contains("not authorized") {
        "Your AWS identity isn't allowed to read load balancers. It needs elasticloadbalancing:Describe* permissions."
            .into()
    } else if e.contains("Unable to locate credentials") || e.contains("could not be found") {
        "No AWS credentials were found for cloud checks. Set a profile in Tessera's settings or in your kubeconfig."
            .into()
    } else {
        format!("AWS CLI error: {}", e.lines().last().unwrap_or(e))
    }
}

/// Extract the AWS region from a load balancer DNS name.
pub fn region_from_dns(dns: &str) -> Option<String> {
    dns.split('.')
        .find(|t| {
            let parts: Vec<&str> = t.split('-').collect();
            parts.len() >= 3
                && parts[0].len() == 2
                && parts[0].chars().all(|c| c.is_ascii_lowercase())
                && parts.last().is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
        .map(String::from)
}

fn norm_dns(s: &str) -> String {
    s.trim_end_matches('.').trim_start_matches("dualstack.").to_lowercase()
}

fn resolve(g: &ClusterGraph, target_type: &str, id: &str) -> Option<String> {
    if target_type == "instance" {
        g.nodes.iter().find(|n| n.instance_id.as_deref() == Some(id)).map(|n| format!("node:{}", n.name))
    } else {
        g.pods.iter().find(|p| p.pod_ip.as_deref() == Some(id)).map(|p| format!("{}/{}", p.namespace, p.name))
    }
}

/// Load balancers the cluster points at, with the object that owns each.
pub fn aws_load_balancers(g: &ClusterGraph) -> Vec<(Target, String)> {
    let mut out: Vec<(Target, String)> = Vec::new();
    let mut push = |t: Target, dns: &String| {
        if dns.contains(".elb.") && dns.ends_with("amazonaws.com") && !out.iter().any(|(_, d)| d == dns) {
            out.push((t, dns.clone()));
        }
    };
    for i in &g.ingresses {
        for a in &i.addresses {
            push(Target { kind: "Ingress".into(), namespace: i.namespace.clone(), name: i.name.clone() }, a);
        }
    }
    for s in g.services.iter().filter(|s| s.type_ == "LoadBalancer") {
        for a in &s.external {
            push(Target { kind: "Service".into(), namespace: s.namespace.clone(), name: s.name.clone() }, a);
        }
    }
    out
}

pub async fn load_balancer_health(g: &ClusterGraph, env: &AwsEnv) -> Vec<LbHealth> {
    let lbs = aws_load_balancers(g);
    let mut out = Vec::new();
    let mut v2_cache: HashMap<String, Result<Value, String>> = HashMap::new();
    let mut v1_cache: HashMap<String, Result<Value, String>> = HashMap::new();
    for (source, dns) in lbs {
        let mut h = LbHealth { provider: "aws".into(), source, dns_name: dns.clone(), ..Default::default() };
        let Some(region) = region_from_dns(&dns).or_else(|| env.region.clone()) else {
            h.error = Some(format!("Couldn't tell which AWS region {dns} is in. Set a region in settings."));
            out.push(h);
            continue;
        };
        if !v2_cache.contains_key(&region) {
            v2_cache.insert(region.clone(), aws(env, &region, &["elbv2", "describe-load-balancers"]).await);
        }
        let v2 = v2_cache[&region].clone();
        let found = match &v2 {
            Ok(v) => v["LoadBalancers"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|lb| lb["DNSName"].as_str().map(norm_dns) == Some(norm_dns(&dns)))
                .cloned(),
            Err(e) => {
                h.error = Some(e.clone());
                out.push(h);
                continue;
            }
        };
        if let Some(lb) = found {
            h.lb_name = lb["LoadBalancerName"].as_str().unwrap_or_default().into();
            let arn = lb["LoadBalancerArn"].as_str().unwrap_or_default();
            match aws(env, &region, &["elbv2", "describe-target-groups", "--load-balancer-arn", arn]).await {
                Ok(tgs) => {
                    for tg in tgs["TargetGroups"].as_array().into_iter().flatten() {
                        let tg_arn = tg["TargetGroupArn"].as_str().unwrap_or_default();
                        let target_type = tg["TargetType"].as_str().unwrap_or("instance").to_string();
                        let codes = tg["Matcher"]["HttpCode"].as_str().or(tg["Matcher"]["GrpcCode"].as_str());
                        let hc = format!(
                            "{}{} on port {}{}",
                            tg["HealthCheckProtocol"].as_str().unwrap_or("TCP"),
                            tg["HealthCheckPath"].as_str().map(|p| format!(" {p}")).unwrap_or_default(),
                            tg["HealthCheckPort"].as_str().unwrap_or("traffic-port"),
                            codes.map(|c| format!(", expects {c}")).unwrap_or_default()
                        );
                        let targets =
                            match aws(env, &region, &["elbv2", "describe-target-health", "--target-group-arn", tg_arn])
                                .await
                            {
                                Ok(th) => th["TargetHealthDescriptions"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .map(|d| {
                                        let id = d["Target"]["Id"].as_str().unwrap_or_default().to_string();
                                        TargetHealth {
                                            resolved: resolve(g, &target_type, &id),
                                            id,
                                            port: d["Target"]["Port"].as_i64().map(|p| p as i32),
                                            state: d["TargetHealth"]["State"].as_str().unwrap_or("unknown").into(),
                                            reason: d["TargetHealth"]["Reason"].as_str().map(String::from),
                                            description: d["TargetHealth"]["Description"].as_str().map(String::from),
                                        }
                                    })
                                    .collect(),
                                Err(e) => {
                                    h.error = Some(e);
                                    vec![]
                                }
                            };
                        h.target_groups.push(TargetGroupHealth {
                            name: tg["TargetGroupName"].as_str().unwrap_or_default().into(),
                            target_type,
                            port: tg["Port"].as_i64().map(|p| p as i32),
                            health_check: hc,
                            targets,
                        });
                    }
                }
                Err(e) => h.error = Some(e),
            }
            out.push(h);
            continue;
        }
        // Classic ELB (in-tree cloud provider, older clusters).
        if !v1_cache.contains_key(&region) {
            v1_cache.insert(region.clone(), aws(env, &region, &["elb", "describe-load-balancers"]).await);
        }
        let classic = v1_cache[&region].as_ref().ok().and_then(|v| {
            v["LoadBalancerDescriptions"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|lb| lb["DNSName"].as_str().map(norm_dns) == Some(norm_dns(&dns)))
                .cloned()
        });
        match classic {
            Some(lb) => {
                let name = lb["LoadBalancerName"].as_str().unwrap_or_default().to_string();
                h.lb_name = name.clone();
                match aws(env, &region, &["elb", "describe-instance-health", "--load-balancer-name", &name]).await {
                    Ok(v) => h.target_groups.push(TargetGroupHealth {
                        name: name.clone(),
                        target_type: "instance".into(),
                        port: None,
                        health_check: lb["HealthCheck"]["Target"].as_str().unwrap_or_default().into(),
                        targets: v["InstanceStates"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|s| {
                                let id = s["InstanceId"].as_str().unwrap_or_default().to_string();
                                TargetHealth {
                                    resolved: resolve(g, "instance", &id),
                                    id,
                                    port: None,
                                    state: if s["State"].as_str() == Some("InService") {
                                        "healthy"
                                    } else {
                                        "unhealthy"
                                    }
                                    .into(),
                                    reason: s["ReasonCode"].as_str().filter(|r| *r != "N/A").map(String::from),
                                    description: s["Description"].as_str().filter(|d| *d != "N/A").map(String::from),
                                }
                            })
                            .collect(),
                    }),
                    Err(e) => h.error = Some(e),
                }
            }
            None => {
                h.error = Some(format!("Couldn't find the load balancer {dns} in {region} with your AWS identity."))
            }
        }
        out.push(h);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions() {
        assert_eq!(
            region_from_dns("k8s-web-edge-1a2b3c-123456.us-east-1.elb.amazonaws.com").as_deref(),
            Some("us-east-1")
        );
        assert_eq!(region_from_dns("k8s-web-nlb-abc.elb.eu-west-2.amazonaws.com").as_deref(), Some("eu-west-2"));
        assert_eq!(region_from_dns("a1b2c3-99.us-gov-west-1.elb.amazonaws.com").as_deref(), Some("us-gov-west-1"));
        assert_eq!(region_from_dns("example.com"), None);
    }

    #[test]
    fn errors_are_explained() {
        assert!(explain_aws_error("An error occurred (ExpiredToken) when calling").contains("aws sso login"));
        assert!(explain_aws_error("An error occurred (AccessDenied)").contains("Describe"));
    }
}
