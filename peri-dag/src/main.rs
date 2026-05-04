use std::sync::{Arc, RwLock};

use axum::{routing::get, routing::post, Router};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{fmt, EnvFilter};

use peri_dag::watcher;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fmt().with_env_filter(EnvFilter::from_default_env()).init();

    // Parse CLI args
    let cli = parse_cli_args();

    // Database
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:peri-dag.db?mode=rwc".to_string());
    let pool = Arc::new(peri_dag::db::init(&database_url).await?);

    let templates = Arc::new(RwLock::new(Vec::new()));

    let state = Arc::new(peri_dag::api::AppState {
        pool: pool.clone(),
        templates: templates.clone(),
    });

    // Start workflow directory watcher if configured
    if let Some(ref workflow_dir) = cli.workflow_dir {
        tracing::info!(dir = %workflow_dir, "starting workflow directory watcher");
        tokio::spawn(watcher::watch_directory(
            pool,
            templates,
            workflow_dir.clone(),
        ));
    }

    // Router
    let app = Router::new()
        // Template API
        .route("/api/v1/templates", get(peri_dag::api::list_templates))
        .route(
            "/api/v1/templates/{name}/run",
            post(peri_dag::api::run_template),
        )
        // Workflow API
        .route("/api/v1/workflows", post(peri_dag::api::submit_workflow))
        .route("/api/v1/workflows", get(peri_dag::api::list_workflows))
        .route(
            "/api/v1/workflows/{run_id}",
            get(peri_dag::api::get_workflow),
        )
        .route(
            "/api/v1/workflows/{run_id}/nodes/{node_id}/logs",
            get(peri_dag::api::get_node_logs),
        )
        // Frontend: serve / as index.html, then all static assets
        .fallback_service(
            ServeDir::new(concat!(env!("CARGO_MANIFEST_DIR"), "/static"))
                .append_index_html_on_directories(true),
        )
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let addr = format!("0.0.0.0:{port}");

    tracing::info!("peri-dag http://{addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

struct CliArgs {
    workflow_dir: Option<String>,
}

fn parse_cli_args() -> CliArgs {
    let args: Vec<String> = std::env::args().collect();
    let mut workflow_dir = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--workflow-dir" => {
                if i + 1 < args.len() {
                    workflow_dir = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--help" | "-h" => {
                eprintln!("peri-dag — DAG workflow engine\n");
                eprintln!("USAGE:");
                eprintln!("    peri-dag [OPTIONS]\n");
                eprintln!("OPTIONS:");
                eprintln!("    --workflow-dir <DIR>  Watch directory for workflow YAML files");
                eprintln!("    --help, -h            Show this help message\n");
                eprintln!("ENVIRONMENT:");
                eprintln!("    DATABASE_URL  SQLite connection string (default: sqlite:peri-dag.db?mode=rwc)");
                eprintln!("    PORT          HTTP server port (default: 3000)");
                eprintln!("    RUST_LOG      Log level (default: info)");
                std::process::exit(0);
            }
            _ => {
                if args[i].starts_with('-') {
                    tracing::warn!(arg = %args[i], "unknown argument, try --help");
                }
            }
        }
        i += 1;
    }
    CliArgs { workflow_dir }
}
