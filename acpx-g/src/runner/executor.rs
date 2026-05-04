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

/// Maximum stdout/stderr length stored per node (256 KB).
/// Longer output is truncated with a marker.
const MAX_STORED_OUTPUT: usize = 256 * 1024;

fn truncate_for_storage(s: &str) -> String {
    if s.len() <= MAX_STORED_OUTPUT {
        return s.to_string();
    }
    let mut end = MAX_STORED_OUTPUT;
    while !s.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    format!("{}\n\n... [truncated, {} bytes total]", &s[..end], s.len())
}

/// Execute a single node and persist results to DB.
/// Returns the resolved outputs map on success.
/// `default_timeout` and `default_retry` from `NodeDefaults` are used when the
/// node's own `ExecConfig` doesn't specify a value.
pub async fn execute_node(
    pool: &SqlitePool,
    run_id: &str,
    node: &NodeDef,
    ctx: &TemplateContext,
    default_timeout: u64,
    default_retry: u32,
) -> anyhow::Result<HashMap<String, String>> {
    let nid = node_id(node);
    let node_run = NodeRun::find_by_run_and_node(pool, run_id, nid)
        .await?
        .ok_or_else(|| anyhow::anyhow!("node_run not found for run={run_id} node={nid}"))?;
    let node_run_id = node_run.id.clone();

    let platform = Platform::detect();

    let result = match node {
        NodeDef::Shell(shell) => {
            let resolved = shell.run.resolve(platform)?;
            let raw_script = load_script(&resolved)?;
            let script = interpolate(&raw_script, ctx);
            let env = build_env(&shell.env, ctx);
            let timeout = shell.exec.timeout.unwrap_or(default_timeout);
            let retry = shell.exec.retry.unwrap_or(default_retry);
            let shell_cmd = shell.exec.shell.clone();
            run_shell(
                pool,
                &node_run_id,
                &script,
                &env,
                Some(timeout),
                Some(retry),
                shell_cmd.as_deref(),
            )
            .await
        }
        NodeDef::Agent(agent) => {
            let resolved = agent.prompt.resolve(platform)?;
            let raw_prompt = load_prompt(&resolved)?;
            let prompt = interpolate(&raw_prompt, ctx);
            let cwd = agent.cwd.as_deref().map(|c| interpolate(c, ctx));
            let env = build_env(&agent.env, ctx);
            let timeout = agent.exec.timeout.unwrap_or(default_timeout);
            let retry = agent.exec.retry.unwrap_or(default_retry);
            run_agent(
                pool,
                &node_run_id,
                &prompt,
                agent.agent.as_deref(),
                agent.model.as_deref(),
                cwd.as_deref(),
                &env,
                Some(timeout),
                Some(retry),
            )
            .await
        }
        NodeDef::Reference(_) => {
            anyhow::bail!("unexpected reference node (should be expanded at load time)")
        }
    };

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

// ─── Generic Retry Executor ──────────────────────────────────────

/// Result of a single command execution attempt.
struct AttemptResult {
    exit_code: i64,
    stdout: String,
    stderr: String,
}

/// Generic retry loop: execute a command with exponential backoff.
/// Accumulates stdout/stderr across attempts and persists state to DB.
async fn execute_with_retry(
    pool: &SqlitePool,
    node_run_id: &str,
    retries: Option<u32>,
    execute_fn: impl Fn() -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<AttemptResult>> + Send>,
    >,
) -> anyhow::Result<()> {
    let max_attempts = retries.unwrap_or(0) + 1;
    let mut last_error = None;
    let mut accumulated_stdout = String::new();
    let mut accumulated_stderr = String::new();

    for attempt in 0..max_attempts {
        sqlx::query("UPDATE node_runs SET attempt = ? WHERE id = ?")
            .bind(attempt as i64)
            .bind(node_run_id)
            .execute(pool)
            .await?;

        NodeRun::set_started(pool, node_run_id).await?;

        match execute_fn().await {
            Ok(result) => {
                if attempt > 0 {
                    accumulated_stdout.push_str(&format!("--- Attempt {} ---\n", attempt + 1));
                    accumulated_stderr.push_str(&format!("--- Attempt {} ---\n", attempt + 1));
                }
                accumulated_stdout.push_str(&result.stdout);
                accumulated_stderr.push_str(&result.stderr);

                let status = if result.exit_code == 0 {
                    "success"
                } else {
                    "failed"
                };
                NodeRun::update_result(
                    pool,
                    node_run_id,
                    status,
                    Some(result.exit_code),
                    Some(&truncate_for_storage(&accumulated_stdout)),
                    Some(&truncate_for_storage(&accumulated_stderr)),
                    None,
                )
                .await?;

                if result.exit_code == 0 {
                    return Ok(());
                }
                last_error = Some(anyhow::anyhow!(
                    "command exited with code {}\nstderr: {}",
                    result.exit_code,
                    result.stderr
                ));
            }
            Err(e) => {
                last_error = Some(e);
                // Cap backoff at 60s to prevent overflow on high retry counts
                let backoff_secs = 1u64.checked_shl(attempt).unwrap_or(60).min(60);
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
            }
        }
    }

    let err = last_error.unwrap_or_else(|| anyhow::anyhow!("execution failed"));
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

// ─── Shell Execution ──────────────────────────────────────────────

async fn run_shell(
    pool: &SqlitePool,
    node_run_id: &str,
    script: &str,
    env: &HashMap<String, String>,
    timeout_secs: Option<u64>,
    retries: Option<u32>,
    shell_override: Option<&str>,
) -> anyhow::Result<()> {
    let script = script.to_string();
    let env = env.clone();
    let shell_override = shell_override.map(|s| s.to_string());

    execute_with_retry(pool, node_run_id, retries, move || {
        let script = script.clone();
        let env = env.clone();
        let shell_override = shell_override.clone();
        Box::pin(async move {
            let (exit_code, stdout, stderr) =
                execute_shell_command(&script, &env, timeout_secs, shell_override.as_deref())
                    .await?;
            Ok(AttemptResult {
                exit_code,
                stdout,
                stderr,
            })
        })
    })
    .await
}

async fn execute_shell_command(
    script: &str,
    env: &HashMap<String, String>,
    timeout_secs: Option<u64>,
    shell_override: Option<&str>,
) -> anyhow::Result<(i64, String, String)> {
    let mut cmd = if let Some(shell) = shell_override {
        // Parse shell override like "bash -c", "sh -c", "zsh -c", etc.
        let parts: Vec<&str> = shell.split_whitespace().collect();
        if parts.is_empty() {
            let mut c = Command::new("bash");
            c.arg("-c");
            c
        } else {
            let mut c = Command::new(parts[0]);
            for arg in &parts[1..] {
                c.arg(*arg);
            }
            c
        }
    } else if cfg!(target_os = "windows") {
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
        match tokio_timeout(Duration::from_secs(secs), cmd.output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(e).context("shell command failed"),
            Err(_) => {
                return Err(anyhow::anyhow!("shell command timed out after {}s", secs));
            }
        }
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
    let prompt = prompt.to_string();
    let agent_name = agent_name.map(|s| s.to_string());
    let model = model.map(|s| s.to_string());
    let cwd = cwd.map(|s| s.to_string());
    let env = env.clone();

    execute_with_retry(pool, node_run_id, retries, move || {
        let prompt = prompt.clone();
        let agent_name = agent_name.clone();
        let model = model.clone();
        let cwd = cwd.clone();
        let env = env.clone();
        Box::pin(async move {
            let (exit_code, stdout, stderr) = execute_agent_command(
                &prompt,
                agent_name.as_deref(),
                model.as_deref(),
                cwd.as_deref(),
                &env,
                timeout_secs,
            )
            .await?;
            Ok(AttemptResult {
                exit_code,
                stdout,
                stderr,
            })
        })
    })
    .await
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
        match tokio_timeout(Duration::from_secs(secs), cmd.output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(e).context("agent command failed"),
            Err(_) => {
                return Err(anyhow::anyhow!("agent command timed out after {}s", secs));
            }
        }
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
    for (k, v) in std::env::vars() {
        env.insert(k, v);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_script_inline() {
        let resolved = crate::schema::ResolvedScript::Inline("echo hello".to_string());
        assert_eq!(load_script(&resolved).unwrap(), "echo hello");
    }

    #[test]
    fn test_load_prompt_inline() {
        let resolved = crate::schema::ResolvedPrompt::Inline("review code".to_string());
        assert_eq!(load_prompt(&resolved).unwrap(), "review code");
    }

    #[test]
    fn test_load_script_file_not_found() {
        let resolved = crate::schema::ResolvedScript::File("/nonexistent/path.sh".to_string());
        assert!(load_script(&resolved).is_err());
    }

    #[test]
    fn test_build_env_inherits_process() {
        let node_env = HashMap::new();
        let ctx = TemplateContext {
            inputs: HashMap::new(),
            needs_outputs: HashMap::new(),
            env: HashMap::new(),
        };
        let env = build_env(&node_env, &ctx);
        // Should contain at least PATH
        assert!(env.contains_key("PATH"));
    }

    #[test]
    fn test_build_env_merges_node_env() {
        let mut node_env = HashMap::new();
        node_env.insert("CUSTOM_VAR".to_string(), "custom_value".to_string());
        let ctx = TemplateContext {
            inputs: HashMap::new(),
            needs_outputs: HashMap::new(),
            env: HashMap::new(),
        };
        let env = build_env(&node_env, &ctx);
        assert_eq!(env.get("CUSTOM_VAR").unwrap(), "custom_value");
    }

    #[test]
    fn test_build_env_interpolates_template() {
        let mut node_env = HashMap::new();
        node_env.insert("DEPLOY_ENV".to_string(), "{{ inputs.env }}".to_string());
        let mut inputs = HashMap::new();
        inputs.insert("env".to_string(), "production".to_string());
        let ctx = TemplateContext {
            inputs,
            needs_outputs: HashMap::new(),
            env: HashMap::new(),
        };
        let env = build_env(&node_env, &ctx);
        assert_eq!(env.get("DEPLOY_ENV").unwrap(), "production");
    }

    #[test]
    fn test_node_id_shell() {
        let node = NodeDef::Shell(crate::schema::ShellNode {
            id: "build".into(),
            run: crate::schema::ScriptSource::Inline("echo".into()),
            depends: vec![],
            outputs: Default::default(),
            env: Default::default(),
            continue_on_error: false,
            exec: crate::schema::ExecConfig {
                timeout: None,
                retry: None,
                shell: None,
            },
        });
        assert_eq!(node_id(&node), "build");
    }

    #[test]
    fn test_node_id_agent() {
        let node = NodeDef::Agent(crate::schema::AgentNode {
            id: "review".into(),
            prompt: crate::schema::PromptSource::Inline("review code".into()),
            agent: None,
            model: None,
            cwd: None,
            depends: vec![],
            outputs: Default::default(),
            env: Default::default(),
            continue_on_error: false,
            exec: crate::schema::ExecConfig {
                timeout: None,
                retry: None,
                shell: None,
            },
        });
        assert_eq!(node_id(&node), "review");
    }

    #[test]
    fn test_get_node_outputs_empty() {
        let node = NodeDef::Shell(crate::schema::ShellNode {
            id: "build".into(),
            run: crate::schema::ScriptSource::Inline("echo".into()),
            depends: vec![],
            outputs: Default::default(),
            env: Default::default(),
            continue_on_error: false,
            exec: crate::schema::ExecConfig {
                timeout: None,
                retry: None,
                shell: None,
            },
        });
        let ctx = TemplateContext {
            inputs: HashMap::new(),
            needs_outputs: HashMap::new(),
            env: HashMap::new(),
        };
        let outputs = get_node_outputs(&node, &ctx);
        assert!(outputs.is_empty());
    }

    #[test]
    fn test_get_node_outputs_interpolated() {
        let mut node_outputs = HashMap::new();
        node_outputs.insert(
            "artifact".to_string(),
            "build/{{ inputs.name }}.tar.gz".to_string(),
        );
        let node = NodeDef::Shell(crate::schema::ShellNode {
            id: "build".into(),
            run: crate::schema::ScriptSource::Inline("echo".into()),
            depends: vec![],
            outputs: node_outputs,
            env: Default::default(),
            continue_on_error: false,
            exec: crate::schema::ExecConfig {
                timeout: None,
                retry: None,
                shell: None,
            },
        });
        let mut inputs = HashMap::new();
        inputs.insert("name".to_string(), "myapp".to_string());
        let ctx = TemplateContext {
            inputs,
            needs_outputs: HashMap::new(),
            env: HashMap::new(),
        };
        let outputs = get_node_outputs(&node, &ctx);
        assert_eq!(outputs.get("artifact").unwrap(), "build/myapp.tar.gz");
    }

    #[test]
    fn test_truncate_short() {
        let s = "hello world";
        assert_eq!(truncate_for_storage(s), "hello world");
    }

    #[test]
    fn test_truncate_exact_limit() {
        let s: String = "a".repeat(MAX_STORED_OUTPUT);
        assert_eq!(truncate_for_storage(&s).len(), MAX_STORED_OUTPUT);
    }

    #[test]
    fn test_truncate_over_limit() {
        let s: String = "a".repeat(MAX_STORED_OUTPUT + 1000);
        let truncated = truncate_for_storage(&s);
        assert!(truncated.len() < s.len());
        assert!(truncated.contains("[truncated"));
    }

    #[test]
    fn test_truncate_multibyte_boundary() {
        // CJK chars are 3 bytes each — make sure we don't slice mid-char
        let s: String = "你".repeat(MAX_STORED_OUTPUT / 3 + 100);
        let truncated = truncate_for_storage(&s);
        assert!(truncated.contains("[truncated"));
        // Verify result is valid UTF-8 (no panic from char boundary slice)
        let _ = truncated.chars().count();
    }

    #[tokio::test]
    async fn test_execute_shell_command_default_shell() {
        let env = HashMap::new();
        let result = execute_shell_command("echo hello_shell_test", &env, Some(10), None).await;
        assert!(result.is_ok());
        let (code, stdout, _stderr) = result.unwrap();
        assert_eq!(code, 0);
        assert!(stdout.contains("hello_shell_test"));
    }

    #[tokio::test]
    async fn test_execute_shell_command_custom_shell() {
        let env = HashMap::new();
        // Override shell to "bash -c"
        let result =
            execute_shell_command("echo custom_shell_test", &env, Some(10), Some("bash -c")).await;
        assert!(result.is_ok());
        let (code, stdout, _stderr) = result.unwrap();
        assert_eq!(code, 0);
        assert!(stdout.contains("custom_shell_test"));
    }

    #[tokio::test]
    async fn test_execute_shell_command_timeout() {
        let env = HashMap::new();
        // This should timeout — sleep 10s with 1s timeout
        let result = execute_shell_command("sleep 10", &env, Some(1), None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_execute_shell_command_nonzero_exit() {
        let env = HashMap::new();
        let result = execute_shell_command("exit 42", &env, Some(5), None).await;
        assert!(result.is_ok());
        let (code, _, _) = result.unwrap();
        assert_eq!(code, 42);
    }

    #[test]
    fn test_max_stored_output_constant() {
        assert_eq!(MAX_STORED_OUTPUT, 256 * 1024);
    }
}
