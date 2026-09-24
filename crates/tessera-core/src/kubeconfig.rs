//! Kubeconfig discovery and per-context clients.
//!
//! Reads `$KUBECONFIG` (all files, merged) or `~/.kube/config`, exactly like
//! kubectl, including exec credential plugins such as `aws eks get-token`.

use crate::{Error, Result};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, Config};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextInfo {
    pub name: String,
    /// Detected from the context name using the policy's patterns.
    pub environment: crate::policy::Environment,
    pub cluster: String,
    pub user: Option<String>,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Contexts {
    pub current: Option<String>,
    pub contexts: Vec<ContextInfo>,
}

/// Contexts the policy allows, each labelled with its environment.
pub fn list_contexts(policy: &crate::policy::Policy) -> Result<Contexts> {
    let kc = Kubeconfig::read().map_err(|e| {
        Error::Kubeconfig(format!(
            "Couldn't read a kubeconfig. Tessera looks at $KUBECONFIG, then ~/.kube/config. ({e})"
        ))
    })?;
    let contexts = kc
        .contexts
        .iter()
        .map(|nc| {
            let ctx = nc.context.as_ref();
            ContextInfo {
                environment: policy.classify(&nc.name),
                name: nc.name.clone(),
                cluster: ctx.map(|c| c.cluster.clone()).unwrap_or_default(),
                user: ctx.and_then(|c| c.user.clone()),
                namespace: ctx.and_then(|c| c.namespace.clone()),
            }
        })
        .filter(|c: &ContextInfo| policy.context_allowed(&c.name))
        .collect();
    Ok(Contexts { current: kc.current_context.clone(), contexts })
}

pub async fn client_for(context: &str) -> Result<Client> {
    let opts = KubeConfigOptions { context: Some(context.to_string()), ..Default::default() };
    let config = Config::from_kubeconfig(&opts)
        .await
        .map_err(|e| Error::Kubeconfig(format!("Couldn't load context {context}: {e}")))?;
    Ok(Client::try_from(config)?)
}
