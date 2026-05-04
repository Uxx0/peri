use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use sqlx::SqlitePool;
use uuid::Uuid;

use crate::api::{TemplateNodeInfo, WorkflowTemplate};
use crate::db::{NodeRun, WorkflowRun};
use crate::runner;
use crate::schema::parse_workflow;

/// Track the latest seen version for each workflow name.
#[derive(Default)]
struct VersionTracker {
    /// workflow_name → latest workflow_version
    versions: HashMap<String, String>,
    /// True until first scan completes (only tracks versions, no submission).
    is_first_scan: bool,
}

/// Periodically scan a directory for .yaml/.yml workflow files.
/// First scan only tracks versions (no submission).
/// Subsequent scans submit new runs only for workflows whose version has changed.
/// The shared `templates` list is refreshed on every scan for the API.
pub async fn watch_directory(
    pool: Arc<SqlitePool>,
    templates: Arc<RwLock<Vec<WorkflowTemplate>>>,
    dir_path: String,
) {
    let mut tracker = VersionTracker {
        versions: HashMap::new(),
        is_first_scan: true,
    };
    let mut interval = tokio::time::interval(Duration::from_secs(10));

    // Immediate first scan (track only, no submits)
    run_scan(&pool, &templates, &dir_path, &mut tracker).await;

    loop {
        interval.tick().await;
        run_scan(&pool, &templates, &dir_path, &mut tracker).await;
    }
}

async fn run_scan(
    pool: &Arc<SqlitePool>,
    templates: &Arc<RwLock<Vec<WorkflowTemplate>>>,
    dir_path: &str,
    tracker: &mut VersionTracker,
) {
    let dir = Path::new(dir_path);
    if !dir.is_dir() {
        tracing::warn!(dir = %dir_path, "workflow watch directory does not exist");
        return;
    }

    let mut scanned_files = 0;
    let mut new_runs = 0u32;
    let first = tracker.is_first_scan;
    let mut template_list = Vec::new();

    match scan_yaml_files(dir) {
        Ok(files) => {
            for file_path in files {
                scanned_files += 1;
                let content = match std::fs::read_to_string(&file_path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(path = %file_path, error = %e, "failed to read workflow file");
                        continue;
                    }
                };

                let wf = match parse_workflow(&content) {
                    Ok(w) => w,
                    Err(e) => {
                        tracing::warn!(path = %file_path, error = %e, "failed to parse workflow file");
                        continue;
                    }
                };

                let name = wf.name.clone();
                let version = wf.version.clone();

                // Always add to template list
                let tpl_nodes: Vec<TemplateNodeInfo> = wf
                    .nodes
                    .iter()
                    .map(|n| TemplateNodeInfo {
                        id: runner::node_id(n).to_string(),
                        node_type: runner::node_type_name(n).to_string(),
                        depends: runner::node_depends(n).to_vec(),
                    })
                    .collect();

                let tpl_inputs: std::collections::HashMap<String, crate::api::TemplateInputDef> =
                    wf.inputs
                        .iter()
                        .map(|(k, def)| {
                            (
                                k.clone(),
                                crate::api::TemplateInputDef {
                                    input_type: match def.input_type {
                                        crate::schema::InputType::String => "string".to_string(),
                                        crate::schema::InputType::Number => "number".to_string(),
                                        crate::schema::InputType::Boolean => "boolean".to_string(),
                                    },
                                    default: def.default.clone(),
                                    required: def.required,
                                },
                            )
                        })
                        .collect();

                template_list.push(WorkflowTemplate {
                    name: name.clone(),
                    version,
                    description: wf.description.clone(),
                    node_count: wf.nodes.len(),
                    file_path: file_path.clone(),
                    nodes: tpl_nodes,
                    inputs: tpl_inputs,
                });

                if first {
                    // First scan: only track versions, don't submit
                    tracker.versions.insert(name, wf.version);
                } else {
                    // Subsequent scans: submit on version change or new workflow
                    let should_submit = match tracker.versions.get(&name) {
                        Some(prev_version) => prev_version != &wf.version,
                        None => true,
                    };

                    if should_submit {
                        if let Err(e) =
                            submit_workflow_from_file(pool, &content, &file_path, &wf).await
                        {
                            tracing::error!(name = %name, error = %e, "failed to submit workflow");
                            continue;
                        }
                        tracker.versions.insert(name, wf.version);
                        new_runs += 1;
                    }
                }
            }
        }
        Err(e) => {
            tracing::error!(dir = %dir_path, error = %e, "failed to scan workflow directory");
            return;
        }
    }

    tracker.is_first_scan = false;

    // Refresh shared template list
    if let Ok(mut lock) = templates.write() {
        *lock = template_list;
    }

    if first {
        tracing::info!(
            dir = %dir_path,
            scanned = scanned_files,
            "workflow directory initial scan complete"
        );
    } else if new_runs > 0 {
        tracing::info!(
            dir = %dir_path,
            new_runs = new_runs,
            "workflow directory detected changes"
        );
    }
}

/// Recursively scan a directory for .yaml and .yml files.
fn scan_yaml_files(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    scan_dir(dir, &mut files)?;
    Ok(files)
}

fn scan_dir(dir: &Path, files: &mut Vec<String>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, files)?;
        } else if let Some(ext) = path.extension() {
            if ext == "yaml" || ext == "yml" {
                files.push(path.to_string_lossy().to_string());
            }
        }
    }
    Ok(())
}

/// Submit a workflow parsed from a file.
/// Expands references via the loader before persisting and executing.
async fn submit_workflow_from_file(
    pool: &SqlitePool,
    yaml_content: &str,
    file_path: &str,
    wf: &crate::schema::Workflow,
) -> anyhow::Result<()> {
    // Expand references using the loader
    let expanded_wf = runner::load_workflow(file_path, std::collections::HashMap::new()).await?;

    let run_id = Uuid::now_v7().to_string();

    let run = WorkflowRun {
        id: run_id.clone(),
        workflow_name: wf.name.clone(),
        workflow_version: wf.version.clone(),
        yaml_content: yaml_content.to_string(),
        status: "pending".to_string(),
        node_count: expanded_wf.nodes.len() as i64,
        started_at: None,
        finished_at: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        error_message: None,
    };

    run.insert(pool).await?;

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
        if let Err(e) = node_run.insert(pool).await {
            tracing::error!(error = %e, "failed to insert node_run");
        }
    }

    let root_inputs = expanded_wf
        .reference_inputs
        .get("__root__")
        .cloned()
        .unwrap_or_default();

    runner::run_workflow(
        Arc::new(pool.clone()),
        run_id.clone(),
        expanded_wf,
        root_inputs,
    )
    .await;

    tracing::info!(
        name = %wf.name,
        version = %wf.version,
        run_id = %run_id,
        "submitted workflow from file"
    );

    Ok(())
}
