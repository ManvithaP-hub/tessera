//! Tauri shell for Tessera. Commands are thin wrappers over `tessera-core`.
//! Guardrails (organisation policy, production protection, audit log) are
//! enforced here in the backend, so the UI can't bypass them.

mod shell_env;

use std::collections::HashMap;
use std::path::PathBuf;
use tauri::{AppHandle, Manager, State};
use tessera_core::active::{NetworkTestReport, NetworkTestRequest, TestPlan};
use tessera_core::audit::{self, AuditEntry};
use tessera_core::policy::{Environment, Policy};
use tessera_core::{Client, ClusterGraph, CollectOptions, Contexts};
use tokio::sync::Mutex;

struct PlannedTest {
    context: String,
    environment: Environment,
    plan: TestPlan,
}

#[derive(Default)]
struct AppState {
    clients: Mutex<HashMap<String, Client>>,
    /// Plans shown to the user, by id. Running a test only accepts an id, so
    /// the webview can never ask the backend to create an arbitrary pod.
    plans: Mutex<HashMap<String, PlannedTest>>,
}

fn env_name(e: Environment) -> &'static str {
    match e {
        Environment::Production => "production",
        Environment::Staging => "staging",
        Environment::Development => "development",
        Environment::Other => "other",
    }
}

fn audit_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("audit.log"))
}

fn record(app: &AppHandle, policy: &Policy, entry: AuditEntry) {
    if !policy.audit_log {
        return;
    }
    if let Some(p) = audit_path(app) {
        let _ = audit::append(&p, &entry);
    }
}

fn check_context(policy: &Policy, context: &str) -> Result<(), String> {
    if policy.context_allowed(context) {
        Ok(())
    } else {
        Err(format!("The context {context} isn't allowed by your Tessera policy."))
    }
}

async fn client(state: &AppState, context: &str) -> Result<Client, String> {
    let mut clients = state.clients.lock().await;
    if let Some(c) = clients.get(context) {
        return Ok(c.clone());
    }
    let c = tessera_core::client_for(context).await.map_err(|e| tessera_core::explain_error(&e.to_string()))?;
    clients.insert(context.to_string(), c.clone());
    Ok(c)
}

async fn forget(state: &AppState, context: &str) {
    state.clients.lock().await.remove(context);
}

#[tauri::command]
fn get_policy() -> Policy {
    Policy::load()
}

#[tauri::command]
fn list_contexts() -> Result<Contexts, String> {
    tessera_core::list_contexts(&Policy::load()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn cluster_snapshot(
    state: State<'_, AppState>,
    context: String,
    options: Option<CollectOptions>,
) -> Result<ClusterGraph, String> {
    let policy = Policy::load();
    check_context(&policy, &context)?;
    let mut options = options.unwrap_or_default();
    options.cloud_checks &= policy.allow_cloud_checks;
    let c = client(&state, &context).await?;
    match tessera_core::collect(c, &context, &options).await {
        Ok(g) => Ok(g),
        Err(e) => {
            // Drop the cached client so the next attempt re-reads the
            // kubeconfig (for example after `aws sso login`).
            forget(&state, &context).await;
            Err(tessera_core::explain_error(&e.to_string()))
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
    check_context(&Policy::load(), &context)?;
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
    environment: Environment,
    /// When true, the run must be confirmed by typing the context name.
    requires_confirmation: bool,
}

/// Build a network test plan (nothing is created yet).
#[tauri::command]
async fn plan_network_test(
    app: AppHandle,
    state: State<'_, AppState>,
    context: String,
    request: NetworkTestRequest,
    environment_override: Option<Environment>,
) -> Result<PlanResponse, String> {
    let policy = Policy::load();
    check_context(&policy, &context)?;
    let environment = policy.classify(&context).stricter(environment_override);
    if let Some(reason) = policy.network_test_blocked(environment) {
        let mut e = AuditEntry::new(&context, env_name(environment), "network_test_blocked", &reason);
        e.namespace = Some(request.namespace.clone());
        e.target = Some(format!("service/{}", request.service));
        record(&app, &policy, e);
        return Err(reason);
    }
    let c = client(&state, &context).await?;
    let g = tessera_core::collect(c, &context, &CollectOptions::default())
        .await
        .map_err(|e| tessera_core::explain_error(&e.to_string()))?;
    let plan = tessera_core::active::plan(&g, &request)?;
    let id = format!(
        "{:x}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    );
    state.plans.lock().await.insert(id.clone(), PlannedTest { context, environment, plan: plan.clone() });
    Ok(PlanResponse { id, plan, environment, requires_confirmation: environment == Environment::Production })
}

/// Run a plan the user approved. Creates the probe pods, reads results, deletes them.
#[tauri::command]
async fn run_network_test(
    app: AppHandle,
    state: State<'_, AppState>,
    plan_id: String,
    confirmation: Option<String>,
) -> Result<NetworkTestReport, String> {
    let planned =
        state.plans.lock().await.remove(&plan_id).ok_or("That test plan has expired. Plan the test again.")?;
    // Re-check policy at run time, in case it changed since planning.
    let policy = Policy::load();
    check_context(&policy, &planned.context)?;
    if let Some(reason) = policy.network_test_blocked(planned.environment) {
        return Err(reason);
    }
    if planned.environment == Environment::Production && confirmation.as_deref() != Some(planned.context.as_str()) {
        return Err("This is a production context. Type the context name exactly to confirm the test.".into());
    }
    let c = client(&state, &planned.context).await?;
    let (ns, svc) = (planned.plan.service.namespace.clone(), planned.plan.service.name.clone());
    let report = tessera_core::active::run(c, planned.plan).await;
    let failing = report.findings.iter().filter(|f| f.status != "ok").count();
    let mut e = AuditEntry::new(
        &planned.context,
        env_name(planned.environment),
        "network_test_run",
        &if failing == 0 { "all checks passed".to_string() } else { format!("{failing} findings") },
    );
    e.namespace = Some(ns);
    e.target = Some(format!("service/{svc}"));
    e.pods_created = report.pods_created.clone();
    e.pods_deleted = report.pods_deleted.clone();
    if e.pods_created.len() != e.pods_deleted.len() {
        e.outcome.push_str("; WARNING: not every probe pod was confirmed deleted");
    }
    record(&app, &policy, e);
    Ok(report)
}

#[tauri::command]
fn audit_log(app: AppHandle, limit: Option<usize>) -> Result<(Vec<AuditEntry>, String), String> {
    let path = audit_path(&app).ok_or("Couldn't find the app data folder.")?;
    Ok((audit::read(&path, limit.unwrap_or(100).min(1000)), path.display().to_string()))
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
            get_policy,
            list_contexts,
            cluster_snapshot,
            pod_logs,
            reset_connection,
            plan_network_test,
            run_network_test,
            audit_log
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tessera");
}
