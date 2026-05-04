use std::sync::{Arc, RwLock};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::db::{
    ListRunsResponse, NodeRun, NodeRunResponse, SubmitWorkflowRequest, SubmitWorkflowResponse,
    WorkflowRun, WorkflowRunResponse,
};
use crate::runner;

/// A workflow template discovered in the watch directory.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkflowTemplate {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub node_count: usize,
    pub file_path: String,
    pub nodes: Vec<TemplateNodeInfo>,
}

/// Lightweight node info for template preview rendering.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TemplateNodeInfo {
    pub id: String,
    pub node_type: String,
    pub depends: Vec<String>,
}

pub struct AppState {
    pub pool: Arc<SqlitePool>,
    pub templates: Arc<RwLock<Vec<WorkflowTemplate>>>,
}

// ─── POST /api/v1/workflows ──────────────────────────────────────

pub async fn submit_workflow(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SubmitWorkflowRequest>,
) -> impl IntoResponse {
    let run_id = Uuid::now_v7().to_string();

    // Parse to get metadata
    let wf = match crate::schema::parse_workflow(&req.yaml) {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("invalid workflow YAML: {e}")})),
            );
        }
    };

    // Validate and apply inputs
    let inputs = match validate_inputs(&wf.inputs, &req.inputs) {
        Ok(i) => i,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("invalid inputs: {e}")})),
            );
        }
    };

    // Load and expand references
    let expanded_wf = match runner::load_workflow_from_content(&req.yaml, inputs).await {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("failed to load workflow: {e}")})),
            );
        }
    };

    // Insert run record
    let run = WorkflowRun {
        id: run_id.clone(),
        workflow_name: wf.name,
        workflow_version: wf.version,
        yaml_content: req.yaml.clone(),
        status: "pending".to_string(),
        node_count: expanded_wf.nodes.len() as i64,
        started_at: None,
        finished_at: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        error_message: None,
    };

    if let Err(e) = run.insert(&state.pool).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("failed to create run: {e}")})),
        );
    }

    // Insert node_run records from expanded workflow
    for node in &expanded_wf.nodes {
        let deps = runner::node_depends(node);
        let node_run = NodeRun {
            id: Uuid::now_v7().to_string(),
            run_id: run_id.clone(),
            node_id: runner::node_id(node).to_string(),
            node_type: runner::node_type_name(node).to_string(),
            status: "pending".to_string(),
            attempt: 0,
            started_at: None,
            finished_at: None,
            exit_code: None,
            stdout: None,
            stderr: None,
            error_message: None,
            outputs: None,
            depends: if deps.is_empty() {
                None
            } else {
                Some(serde_json::to_string(deps).unwrap())
            },
        };
        if let Err(e) = node_run.insert(&state.pool).await {
            tracing::error!(error = %e, "failed to insert node_run");
        }
    }

    // Get root inputs from expanded workflow
    let root_inputs = expanded_wf
        .reference_inputs
        .get("__root__")
        .cloned()
        .unwrap_or_default();

    // Start async execution
    runner::run_workflow(state.pool.clone(), run_id.clone(), expanded_wf, root_inputs).await;

    (
        StatusCode::CREATED,
        Json(serde_json::json!(SubmitWorkflowResponse {
            run_id,
            status: "pending".to_string(),
        })),
    )
}

// ─── GET /api/v1/workflows ───────────────────────────────────────

pub async fn list_workflows(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match WorkflowRun::list(&state.pool, 50).await {
        Ok(runs) => {
            let response_runs: Vec<WorkflowRunResponse> =
                runs.into_iter().map(WorkflowRunResponse::from).collect();
            (
                StatusCode::OK,
                Json(serde_json::json!(ListRunsResponse {
                    runs: response_runs,
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{e}")})),
        ),
    }
}

// ─── GET /api/v1/workflows/:run_id ───────────────────────────────

pub async fn get_workflow(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<String>,
) -> impl IntoResponse {
    match WorkflowRun::find_by_id(&state.pool, &run_id).await {
        Ok(Some(run)) => {
            let nodes = NodeRun::find_by_run(&state.pool, &run_id)
                .await
                .unwrap_or_default();

            // depends info is now stored directly in node_runs table
            // (populated during workflow submission after reference expansion)
            let mut response = WorkflowRunResponse::from(run);
            response.nodes = nodes.into_iter().map(NodeRunResponse::from).collect();
            (StatusCode::OK, Json(serde_json::json!(response)))
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "workflow run not found"})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{e}")})),
        ),
    }
}

// ─── GET /api/v1/workflows/:run_id/nodes/:node_id/logs ───────────

pub async fn get_node_logs(
    State(state): State<Arc<AppState>>,
    Path((run_id, node_run_id)): Path<(String, String)>,
) -> impl IntoResponse {
    match NodeRun::find_by_id(&state.pool, &node_run_id).await {
        Ok(Some(node)) => {
            if node.run_id != run_id {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "node not found in this run"})),
                );
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "node_id": node.node_id,
                    "status": node.status,
                    "attempt": node.attempt,
                    "stdout": node.stdout,
                    "stderr": node.stderr,
                    "exit_code": node.exit_code,
                })),
            )
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "node not found"})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{e}")})),
        ),
    }
}

// ─── GET /api/v1/templates ───────────────────────────────────────

pub async fn list_templates(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let templates = state.templates.read().unwrap().clone();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "templates": templates })),
    )
}

// ─── POST /api/v1/templates/:name/run ────────────────────────────

pub async fn run_template(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let templates = state.templates.read().unwrap().clone();
    let template = match templates.iter().find(|t| t.name == name) {
        Some(t) => t.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "template not found"})),
            );
        }
    };

    let yaml_content = match std::fs::read_to_string(&template.file_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("failed to read template file: {e}")})),
            );
        }
    };

    let wf = match crate::schema::parse_workflow(&yaml_content) {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("invalid workflow: {e}")})),
            );
        }
    };

    // Templates use defaults (no user-provided inputs)
    let inputs = validate_inputs(&wf.inputs, &None).unwrap_or_default();

    // Load and expand references
    let expanded_wf = match runner::load_workflow(&template.file_path, inputs).await {
        Ok(w) => w,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("failed to load workflow: {e}")})),
            );
        }
    };

    let run_id = Uuid::now_v7().to_string();

    let run = WorkflowRun {
        id: run_id.clone(),
        workflow_name: wf.name.clone(),
        workflow_version: wf.version.clone(),
        yaml_content: yaml_content.clone(),
        status: "pending".to_string(),
        node_count: expanded_wf.nodes.len() as i64,
        started_at: None,
        finished_at: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        error_message: None,
    };

    if let Err(e) = run.insert(&state.pool).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("{e}")})),
        );
    }

    for node in &expanded_wf.nodes {
        let deps = runner::node_depends(node);
        let node_run = NodeRun {
            id: Uuid::now_v7().to_string(),
            run_id: run_id.clone(),
            node_id: runner::node_id(node).to_string(),
            node_type: runner::node_type_name(node).to_string(),
            status: "pending".to_string(),
            attempt: 0,
            started_at: None,
            finished_at: None,
            exit_code: None,
            stdout: None,
            stderr: None,
            error_message: None,
            outputs: None,
            depends: if deps.is_empty() {
                None
            } else {
                Some(serde_json::to_string(deps).unwrap())
            },
        };
        if let Err(e) = node_run.insert(&state.pool).await {
            tracing::error!(error = %e, "failed to insert node_run");
        }
    }

    let root_inputs = expanded_wf
        .reference_inputs
        .get("__root__")
        .cloned()
        .unwrap_or_default();

    runner::run_workflow(state.pool.clone(), run_id.clone(), expanded_wf, root_inputs).await;

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "run_id": run_id,
            "status": "pending",
            "template": name,
        })),
    )
}

// ─── Input Validation ─────────────────────────────────────────────

/// Validate provided inputs against declared InputDefs.
/// Returns a fully populated HashMap with defaults applied.
fn validate_inputs(
    declared: &std::collections::HashMap<String, crate::schema::InputDef>,
    provided: &Option<std::collections::HashMap<String, String>>,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut result = std::collections::HashMap::new();
    let provided = provided.as_ref();

    for (key, def) in declared {
        if let Some(val) = provided.and_then(|p| p.get(key)) {
            result.insert(key.clone(), val.clone());
        } else if let Some(default) = &def.default {
            result.insert(key.clone(), default.clone());
        } else if def.required {
            anyhow::bail!("required input '{}' not provided", key);
        }
    }

    Ok(result)
}
