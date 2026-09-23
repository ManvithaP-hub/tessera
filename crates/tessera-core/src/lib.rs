//! Core engine for Tessera: reads a cluster through the user's kubeconfig,
//! builds a traffic-path snapshot, and diagnoses where requests break.
//!
//! This crate never writes to the cluster. Every API call is a `list`,
//! `get`, or a log read.

pub mod collect;
pub mod diagnose;
pub mod extended;
pub mod kubeconfig;
pub mod model;
pub mod quantity;

pub use collect::{build_graph, collect, pod_logs, RawSnapshot};
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
