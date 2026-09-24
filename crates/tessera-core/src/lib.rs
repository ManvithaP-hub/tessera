//! Core engine for Tessera: reads a cluster through the user's kubeconfig,
//! builds a traffic-path snapshot, and diagnoses where requests break.
//!
//! This crate never writes to the cluster. Every API call is a `list`,
//! `get`, or a log read.

pub mod active;
pub mod audit;
pub mod cloud;
pub mod collect;
pub mod diagnose;
pub mod extended;
pub mod kubeconfig;
pub mod model;
pub mod policy;
pub mod quantity;

pub use collect::{build_graph, collect, pod_logs, CollectOptions, RawSnapshot};
pub use diagnose::diagnose;
pub use kube::Client;
pub use kubeconfig::{client_for, list_contexts, ContextInfo, Contexts};
pub use model::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Kubeconfig(String),
    #[error(transparent)]
    Kube(#[from] kube::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Turn a raw client error into something a person can act on. The original
/// text is kept at the end for bug reports.
pub fn explain_error(raw: &str) -> String {
    let l = raw.to_lowercase();
    let msg = if l.contains("expiredtoken") || l.contains("token has expired") || l.contains("sso session") {
        "Your cloud login has expired. Run `aws sso login` (or your provider's login command), then select Refresh."
    } else if l.contains("unauthorized") || l.contains("401") {
        "The cluster rejected your credentials. Check that your kubeconfig user is still valid for this cluster."
    } else if l.contains("forbidden") || l.contains("403") {
        "You're connected, but your credentials aren't allowed to read this cluster. See docs/rbac.yaml for the permissions Tessera needs."
    } else if (l.contains("exec") || l.contains("credential"))
        && (l.contains("no such file") || l.contains("not found"))
    {
        "Couldn't run the login helper your kubeconfig uses (for example `aws` or `gke-gcloud-auth-plugin`). Check it's installed and on your PATH."
    } else if l.contains("dns error")
        || l.contains("failed to lookup")
        || l.contains("nodename nor servname")
        || l.contains("name or service not known")
    {
        "Can't find this cluster's API server address. The cluster may have been deleted, or you may need a VPN or private DNS."
    } else if l.contains("connect")
        || l.contains("connection refused")
        || l.contains("timed out")
        || l.contains("unreachable")
    {
        "Can't reach this cluster's API server. It may have been deleted or stopped, or you may need a VPN or network access."
    } else if l.contains("certificate") || l.contains("tls") {
        "Couldn't verify the cluster's TLS certificate. The kubeconfig may be out of date for this cluster."
    } else {
        return raw.to_string();
    };
    format!("{msg} (Details: {})", raw.trim())
}

#[cfg(test)]
mod tests {
    use super::explain_error;

    #[test]
    fn explains_common_errors() {
        assert!(explain_error("ServiceError: client error (Connect)").starts_with("Can't reach this cluster"));
        assert!(explain_error("dns error: failed to lookup address information").starts_with("Can't find"));
        assert!(explain_error("ApiError: Unauthorized: 401").contains("rejected your credentials"));
        assert!(explain_error("An error occurred (ExpiredToken)").contains("aws sso login"));
        assert!(explain_error("failed to exec auth plugin: No such file or directory").contains("login helper"));
        assert_eq!(explain_error("something else"), "something else");
    }
}
