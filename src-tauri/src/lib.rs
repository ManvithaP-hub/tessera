//! Tauri shell for Tessera. The commands here are thin wrappers over
//! `tessera-core`; they only ever read from the cluster.

mod shell_env;

use std::collections::HashMap;
use tauri::State;
use tessera_core::{Client, ClusterGraph, Contexts};
use tokio::sync::Mutex;

#[derive(Default)]
struct AppState {
    clients: Mutex<HashMap<String, Client>>,
}

async fn client(state: &AppState, context: &str) -> Result<Client, String> {
    let mut clients = state.clients.lock().await;
    if let Some(c) = clients.get(context) {
        return Ok(c.clone());
    }
    let c = tessera_core::client_for(context).await.map_err(|e| e.to_string())?;
    clients.insert(context.to_string(), c.clone());
    Ok(c)
}

async fn forget(state: &AppState, context: &str) {
    state.clients.lock().await.remove(context);
}

#[tauri::command]
fn list_contexts() -> Result<Contexts, String> {
    tessera_core::list_contexts().map_err(|e| e.to_string())
}

#[tauri::command]
async fn cluster_snapshot(state: State<'_, AppState>, context: String) -> Result<ClusterGraph, String> {
    let c = client(&state, &context).await?;
    match tessera_core::collect(c, &context).await {
        Ok(g) => Ok(g),
        Err(e) => {
            // Drop the cached client so the next attempt re-reads the
            // kubeconfig (for example after `aws sso login`).
            forget(&state, &context).await;
            Err(e.to_string())
        }
    }
}

#[tauri::command]
async fn pod_logs(
    state: State<'_, AppState>,
    context: String,
    namespace: String,
    name: String,
    container: Option<String>,
    previous: bool,
    tail: Option<i64>,
) -> Result<String, String> {
    let c = client(&state, &context).await?;
    tessera_core::pod_logs(c, &namespace, &name, container, previous, tail.unwrap_or(500).clamp(10, 5000))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn reset_connection(state: State<'_, AppState>, context: String) -> Result<(), String> {
    forget(&state, &context).await;
    Ok(())
}

pub fn run() {
    shell_env::fix_path();
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![list_contexts, cluster_snapshot, pod_logs, reset_connection])
        .run(tauri::generate_context!())
        .expect("error while running Tessera");
}
