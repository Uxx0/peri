use anyhow::Context;
use sqlx::SqlitePool;
use std::collections::HashMap;
use tokio::process::Command;
use tokio::time::{timeout as tokio_timeout, Duration};

use crate::db::NodeRun;
use crate::runner::template::interpolate;
use crate::runner::template::interpolate_map;
use crate::runner::template::TemplateContext;
use crate::schema::{NodeDef, Platform};

/// Execute a single node and persist results to DB.
/// Returns the resolved outputs map on success.
pub async fn execute_node(
    pool: &SqlitePool,
    run_id: &str,
    node: &NodeDef,
    ctx: &TemplateContext,
) -> anyhow::Result<HashMap<String, String>> {
    let nid = node_id(node);
    let node_run = NodeRun::find_by_run_and_node(pool, run_id, nid)
        .await?
        .ok_or_else(|| anyhow::anyhow!("node_run not found for run={run_id} node={nid}"))?;
    let node_run_id = node_run.id.clone();

    let platform = Platform::detect();

    let result = match node {
        NodeDef::Shell(shell) => {
            // Interpolate script content
            let resolved = shell.run.resolve(platform);
            let raw_script = load_script(&resolved)?;
            let script = interpolate(&raw_script, ctx);
            let env = build_env(&shell.env, ctx);
            run_shell(
                pool,
                &node_run_id,
                &script,
                &env,
                shell.exec.timeout,
                shell.exec.retry,
            )
            .await
        }
        NodeDef::Agent(agent) => {
            let resolved = agent.prompt.resolve(platform);
            let raw_prompt = load_prompt(&resolved)?;
            let prompt = interpolate(&raw_prompt, ctx);
            let cwd = agent.cwd.as_deref().map(|c| interpolate(c, ctx));
            let env = build_env(&agent.env, ctx);
            run_agent(
                pool,
                &node_run_id,
                &prompt,
                agent.agent.as_deref(),
                agent.model.as_deref(),
                cwd.as_deref(),
                &env,
                agent.exec.timeout,
                agent.exec.retry,
            )
            .await
        }
        NodeDef::Reference(_) => {
            // Should not happen — references are expanded at load time
            anyhow::bail!("unexpected reference node (should be expanded at load time)")
        }
    };

    // On success, resolve and persist outputs
    if result.is_ok() {
        let outputs = get_node_outputs(node, ctx);
        if !outputs.is_empty() {
            let outputs_json = serde_json::to_string(&outputs)?;
            NodeRun::update_outputs(pool, &node_run_id, &outputs_json).await?;
        }
        return Ok(outputs);
    }

    result.map(|_| HashMap::new())
}

fn get_node_outputs(node: &NodeDef, ctx: &TemplateContext) -> HashMap<String, String> {
    let raw = match node {
        NodeDef::Shell(n) => &n.outputs,
        NodeDef::Agent(n) => &n.outputs,
        NodeDef::Reference(n) => &n.outputs,
    };
    interpolate_map(raw, ctx)
}

// ─── Shell Execution ──────────────────────────────────────────────

async fn run_shell(
    pool: &SqlitePool,
    node_run_id: &str,
    script: &str,
    env: &HashMap<String, String>,
    timeout_secs: Option<u64>,
    retries: Option<u32>,
) -> anyhow::Result<()> {
    let max_attempts = retries.unwrap_or(0) + 1;
    let mut last_error = None;

    for attempt in 0..max_attempts {
        sqlx::query("UPDATE node_runs SET attempt = ? WHERE id = ?")
            .bind(attempt as i64)
            .bind(node_run_id)
            .execute(pool)
            .await?;

        NodeRun::set_started(pool, node_run_id).await?;

        let result = execute_shell_command(script, env, timeout_secs).await;

        match result {
            Ok((exit_code, stdout, stderr)) => {
                let status = if exit_code == 0 { "success" } else { "failed" };
                NodeRun::update_result(
                    pool,
                    node_run_id,
                    status,
                    Some(exit_code),
                    Some(&stdout),
                    Some(&stderr),
                    None,
                )
                .await?;

                if exit_code == 0 {
                    return Ok(());
                }
                last_error = Some(anyhow::anyhow!(
                    "shell exited with code {exit_code}\nstderr: {stderr}"
                ));
            }
            Err(e) => {
                last_error = Some(e);
                tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
            }
        }
    }

    let err = last_error.unwrap_or_else(|| anyhow::anyhow!("shell execution failed"));
    NodeRun::update_result(
        pool,
        node_run_id,
        "failed",
        None,
        None,
        None,
        Some(&err.to_string()),
    )
    .await?;
    Err(err)
}

async fn execute_shell_command(
    script: &str,
    env: &HashMap<String, String>,
    timeout_secs: Option<u64>,
) -> anyhow::Result<(i64, String, String)> {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.arg("/C");
        c
    } else {
        let mut c = Command::new("bash");
        c.arg("-c");
        c
    };

    cmd.arg(script);
    cmd.envs(env);
    cmd.kill_on_drop(true);

    let output = if let Some(secs) = timeout_secs {
        tokio_timeout(Duration::from_secs(secs), cmd.output())
            .await
            .context("shell command timed out")??
    } else {
        cmd.output().await?
    };

    let exit_code = output.status.code().unwrap_or(-1) as i64;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    Ok((exit_code, stdout, stderr))
}

// ─── Agent Execution ──────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn run_agent(
    pool: &SqlitePool,
    node_run_id: &str,
    prompt: &str,
    agent_name: Option<&str>,
    model: Option<&str>,
    cwd: Option<&str>,
    env: &HashMap<String, String>,
    timeout_secs: Option<u64>,
    retries: Option<u32>,
) -> anyhow::Result<()> {
    let max_attempts = retries.unwrap_or(0) + 1;
    let mut last_error = None;

    for attempt in 0..max_attempts {
        sqlx::query("UPDATE node_runs SET attempt = ? WHERE id = ?")
            .bind(attempt as i64)
            .bind(node_run_id)
            .execute(pool)
            .await?;

        NodeRun::set_started(pool, node_run_id).await?;

        let result = execute_agent_command(prompt, agent_name, model, cwd, env, timeout_secs).await;

        match result {
            Ok((exit_code, stdout, stderr)) => {
                let status = if exit_code == 0 { "success" } else { "failed" };
                NodeRun::update_result(
                    pool,
                    node_run_id,
                    status,
                    Some(exit_code),
                    Some(&stdout),
                    Some(&stderr),
                    None,
                )
                .await?;

                if exit_code == 0 {
                    return Ok(());
                }
                last_error = Some(anyhow::anyhow!(
                    "agent exited with code {exit_code}\nstderr: {stderr}"
                ));
            }
            Err(e) => {
                last_error = Some(e);
                tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
            }
        }
    }

    let err = last_error.unwrap_or_else(|| anyhow::anyhow!("agent execution failed"));
    NodeRun::update_result(
        pool,
        node_run_id,
        "failed",
        None,
        None,
        None,
        Some(&err.to_string()),
    )
    .await?;
    Err(err)
}

async fn execute_agent_command(
    prompt: &str,
    agent_name: Option<&str>,
    model: Option<&str>,
    cwd: Option<&str>,
    env: &HashMap<String, String>,
    timeout_secs: Option<u64>,
) -> anyhow::Result<(i64, String, String)> {
    let agent = agent_name.unwrap_or("peri");
    let mut cmd = Command::new("acpx");
    cmd.arg("--approve-all").arg("--format").arg("text");

    if let Some(model) = model {
        cmd.arg("--model").arg(model);
    }
    if let Some(cwd) = cwd {
        cmd.arg("--cwd").arg(cwd);
    }
    if let Some(secs) = timeout_secs {
        cmd.arg("--timeout").arg(secs.to_string());
    }

    cmd.arg(agent).arg("exec").arg(prompt);

    cmd.envs(env);
    cmd.kill_on_drop(true);

    let output = if let Some(secs) = timeout_secs {
        tokio_timeout(Duration::from_secs(secs), cmd.output())
            .await
            .context("agent command timed out")??
    } else {
        cmd.output().await?
    };

    let exit_code = output.status.code().unwrap_or(-1) as i64;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    Ok((exit_code, stdout, stderr))
}

// ─── Helpers ──────────────────────────────────────────────────────

fn load_script(resolved: &crate::schema::ResolvedScript) -> anyhow::Result<String> {
    match resolved {
        crate::schema::ResolvedScript::Inline(s) => Ok(s.clone()),
        crate::schema::ResolvedScript::File(path) => std::fs::read_to_string(path)
            .with_context(|| format!("failed to read script file: {path}")),
    }
}

fn load_prompt(resolved: &crate::schema::ResolvedPrompt) -> anyhow::Result<String> {
    match resolved {
        crate::schema::ResolvedPrompt::Inline(s) => Ok(s.clone()),
        crate::schema::ResolvedPrompt::File(path) => std::fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file: {path}")),
    }
}

/// Build merged environment: process env + global env (interpolated) + node env (interpolated).
fn build_env(node_env: &HashMap<String, String>, ctx: &TemplateContext) -> HashMap<String, String> {
    let mut env = HashMap::new();
    // Inherit current process env
    for (k, v) in std::env::vars() {
        env.insert(k, v);
    }
    // Interpolate and merge node-specific env
    let resolved = interpolate_map(node_env, ctx);
    for (k, v) in resolved {
        env.insert(k, v);
    }
    env
}

fn node_id(node: &NodeDef) -> &str {
    match node {
        NodeDef::Shell(n) => &n.id,
        NodeDef::Agent(n) => &n.id,
        NodeDef::Reference(n) => &n.id,
    }
}
