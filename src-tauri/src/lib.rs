//! Tauri shell for Tessera. The commands here are thin wrappers over
//! `tessera-core`; they only ever read from the cluster.

mod shell_env;

use std::collections::HashMap;
use tauri::State;
use tessera_core::active::{NetworkTestReport, NetworkTestRequest, TestPlan};
use tessera_core::{Client, ClusterGraph, CollectOptions, Contexts};
use tokio::sync::Mutex;

#[derive(Default)]
struct AppState {
    clients: Mutex<HashMap<String, Client>>,
    /// Plans shown to the user, by id. Running a test only accepts an id, so
    /// the webview can never ask the backend to create an arbitrary pod.
    plans: Mutex<HashMap<String, (String, TestPlan)>>,
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
async fn cluster_snapshot(
    state: State<'_, AppState>,
    context: String,
    options: Option<CollectOptions>,
) -> Result<ClusterGraph, String> {
    let c = client(&state, &context).await?;
    match tessera_core::collect(c, &context, &options.unwrap_or_default()).await {
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

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanResponse {
    id: String,
    plan: TestPlan,
}

/// Build a network test plan (nothing is created yet).
#[tauri::command]
async fn plan_network_test(
    state: State<'_, AppState>,
    context: String,
    request: NetworkTestRequest,
) -> Result<PlanResponse, String> {
    let c = client(&state, &context).await?;
    let g = tessera_core::collect(c, &context, &CollectOptions::default()).await.map_err(|e| e.to_string())?;
    let plan = tessera_core::active::plan(&g, &request)?;
    let id = format!(
        "{:x}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    );
    state.plans.lock().await.insert(id.clone(), (context, plan.clone()));
    Ok(PlanResponse { id, plan })
}

/// Run a plan the user approved. Creates the probe pods, reads results, deletes them.
#[tauri::command]
async fn run_network_test(state: State<'_, AppState>, plan_id: String) -> Result<NetworkTestReport, String> {
    let (context, plan) =
        state.plans.lock().await.remove(&plan_id).ok_or("That test plan has expired. Plan the test again.")?;
    let c = client(&state, &context).await?;
    Ok(tessera_core::active::run(c, plan).await)
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
        .invoke_handler(tauri::generate_handler![
            list_contexts,
            cluster_snapshot,
            pod_logs,
            reset_connection,
            plan_network_test,
            run_network_test
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tessera");
}
