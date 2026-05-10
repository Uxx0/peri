use std::collections::HashSet;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::hooks::output_parser::{parse_command_hook_output, parse_http_hook_response};
use crate::hooks::ssrf_guard::check_url;
use crate::hooks::types::{HookAction, HookInput, HookType, RegisteredHook};
use crate::hooks::variables::resolve_hook_variables;
use rust_create_agent::agent::react::{AgentInput, ReactLLM};
use rust_create_agent::agent::state::AgentState;
use rust_create_agent::agent::ReActAgent;
use rust_create_agent::agent::State;
use rust_create_agent::messages::BaseMessage;

/// Execute a command hook (shell script).
///
/// - shell default "bash", timeout default 600s
/// - stdin: serialized HookInput JSON
/// - exit code 0 → parse stdout, 1 → Allow(warn), 2 → Block(reason)
/// - timeout → Allow(warn)
pub async fn execute_command_hook(
    hook: &HookType,
    input: &HookInput,
    registered: &RegisteredHook,
) -> HookAction {
    let (command, shell, timeout_secs) = match hook {
        HookType::Command {
            command,
            shell,
            timeout,
            ..
        } => (
            command.clone(),
            shell.clone().unwrap_or_else(|| "bash".to_string()),
            timeout.unwrap_or(600),
        ),
        _ => {
            return HookAction::Allow;
        }
    };

    let input_json = match serde_json::to_string(input) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!("Failed to serialize HookInput: {}", e);
            return HookAction::Allow;
        }
    };

    // Resolve ${CLAUDE_PLUGIN_ROOT}, ${CLAUDE_PLUGIN_DATA}, ${ARGUMENTS} in command string
    let command = resolve_hook_variables(
        &command,
        &registered.plugin_root,
        &registered.plugin_data_dir,
        &input_json,
    );

    let plugin_root_str = registered.plugin_root.to_string_lossy().to_string();
    let plugin_data_str = registered.plugin_data_dir.to_string_lossy().to_string();
    let hook_event_str = format!("{:?}", input.hook_event_name);

    let result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        let mut cmd = tokio::process::Command::new(&shell);
        cmd.arg("-c")
            .arg(&command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CLAUDE_PROJECT_DIR", &input.cwd)
            .env("CLAUDE_PLUGIN_ROOT", &plugin_root_str)
            .env("CLAUDE_PLUGIN_DATA", &plugin_data_str)
            .env("CLAUDE_HOOK_EVENT_NAME", &hook_event_str)
            .kill_on_drop(true);

        // Inject CLAUDE_PLUGIN_OPTION_* env vars
        for (key, value) in &registered.plugin_options {
            let env_key = format!("CLAUDE_PLUGIN_OPTION_{}", key.to_uppercase());
            cmd.env(env_key, value.to_string());
        }

        let mut child = cmd.spawn()?;

        // Write input JSON to stdin
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(input_json.as_bytes()).await {
                tracing::warn!("Failed to write to hook stdin: {}", e);
            }
            drop(stdin);
        }

        let output = child.wait_with_output().await?;
        Ok::<_, std::io::Error>(output)
    })
    .await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            match output.status.code() {
                Some(0) => {
                    // Parse structured output
                    parse_command_hook_output(&stdout)
                }
                Some(1) => {
                    // Exit code 1 → Allow with warning
                    if !stderr.is_empty() {
                        tracing::warn!("Command hook exited with code 1: {}", stderr);
                    }
                    HookAction::Allow
                }
                Some(2) => {
                    // Exit code 2 → Block
                    let reason = if !stdout.trim().is_empty() {
                        stdout.trim().to_string()
                    } else if !stderr.trim().is_empty() {
                        stderr.trim().to_string()
                    } else {
                        "Blocked by hook (exit code 2)".to_string()
                    };
                    HookAction::Block { reason }
                }
                Some(code) => {
                    tracing::warn!(
                        "Command hook exited with unexpected code {}: stderr={}",
                        code,
                        stderr
                    );
                    HookAction::Allow
                }
                None => {
                    tracing::warn!("Command hook terminated by signal");
                    HookAction::Allow
                }
            }
        }
        Ok(Err(e)) => {
            tracing::warn!("Command hook execution failed: {}", e);
            HookAction::Allow
        }
        Err(_) => {
            // Timeout
            tracing::warn!(
                "Command hook timed out after {}s: {}",
                timeout_secs,
                command
            );
            HookAction::Allow
        }
    }
}

/// Execute a prompt hook (LLM evaluation).
///
/// - timeout default 30s
/// - Replace $ARGUMENTS in prompt with input JSON
/// - Call llm.generate_reasoning, parse result
pub async fn execute_prompt_hook(
    hook: &HookType,
    input: &HookInput,
    llm_factory: &Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
) -> HookAction {
    let (prompt_template, timeout_secs) = match hook {
        HookType::Prompt {
            prompt, timeout, ..
        } => (prompt.as_str(), timeout.unwrap_or(30)),
        _ => {
            return HookAction::Allow;
        }
    };

    let input_json = match serde_json::to_string(input) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!("Failed to serialize HookInput for prompt hook: {}", e);
            return HookAction::Allow;
        }
    };

    // Replace $ARGUMENTS with input JSON
    let prompt = prompt_template.replace("$ARGUMENTS", &input_json);
    let prompt = prompt.replace("${ARGUMENTS}", &input_json);

    let result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        let llm = llm_factory();
        // Build a minimal message list with just the prompt as a system message
        let messages = vec![BaseMessage::system(prompt.clone())];
        let reasoning = llm.generate_reasoning(&messages, &[]).await?;
        Ok::<_, anyhow::Error>(reasoning)
    })
    .await;

    match result {
        Ok(Ok(reasoning)) => {
            let answer = reasoning
                .final_answer
                .unwrap_or(reasoning.thought)
                .trim()
                .to_string();
            parse_command_hook_output(&answer)
        }
        Ok(Err(e)) => {
            tracing::warn!("Prompt hook LLM call failed: {}", e);
            HookAction::Allow
        }
        Err(_) => {
            tracing::warn!("Prompt hook timed out after {}s", timeout_secs);
            HookAction::Allow
        }
    }
}

/// Execute an HTTP hook (POST request).
///
/// - SSRF guard check first
/// - timeout default 600s
/// - POST with JSON body, CRLF-injection-safe headers
pub async fn execute_http_hook(hook: &HookType, input: &HookInput) -> HookAction {
    let (url, timeout_secs, headers, allowed_env_vars) = match hook {
        HookType::Http {
            url,
            timeout,
            headers,
            allowed_env_vars,
            ..
        } => (
            url.as_str(),
            timeout.unwrap_or(600),
            headers,
            allowed_env_vars,
        ),
        _ => {
            return HookAction::Allow;
        }
    };

    // SSRF guard
    if let Err(reason) = check_url(url) {
        tracing::warn!("HTTP hook blocked by SSRF guard: {}", reason);
        return HookAction::Block {
            reason: format!("SSRF guard blocked URL: {}", reason),
        };
    }

    let input_json = match serde_json::to_string(input) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!("Failed to serialize HookInput for HTTP hook: {}", e);
            return HookAction::Allow;
        }
    };

    // Build allowed_env_vars set for header sanitization
    let allowed_set: HashSet<String> = allowed_env_vars.iter().cloned().collect();

    // Sanitize and build headers
    let mut req_headers = reqwest::header::HeaderMap::new();
    req_headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    for (key, value) in headers {
        let sanitized = sanitize_header_value(value, &allowed_set);
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(key.as_bytes()),
            reqwest::header::HeaderValue::from_str(&sanitized),
        ) {
            req_headers.insert(name, val);
        }
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Failed to build HTTP client: {}", e);
            return HookAction::Allow;
        }
    };

    match client
        .post(url)
        .headers(req_headers)
        .body(input_json)
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();

            if !status.is_success() {
                tracing::warn!(
                    "HTTP hook returned non-success status {}: {}",
                    status,
                    if body.len() > 200 {
                        format!("{}...", &body[..body.floor_char_boundary(200)])
                    } else {
                        body
                    }
                );
                return HookAction::Allow;
            }

            parse_http_hook_response(&body)
        }
        Err(e) => {
            tracing::warn!("HTTP hook request failed: {}", e);
            HookAction::Allow
        }
    }
}

/// Execute an agent hook (full ReAct agent loop).
///
/// - timeout default 60s, max_turns 50
/// - No HookMiddleware, no SubAgentMiddleware (prevent recursion)
/// - After execution, look for structured output in messages
pub async fn execute_agent_hook(
    hook: &HookType,
    input: &HookInput,
    llm_factory: &Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
    cwd: &str,
) -> HookAction {
    let (prompt_template, timeout_secs) = match hook {
        HookType::Agent {
            prompt, timeout, ..
        } => (prompt.as_str(), timeout.unwrap_or(60)),
        _ => {
            return HookAction::Allow;
        }
    };

    let max_turns: usize = 50;

    let input_json = match serde_json::to_string_pretty(input) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!("Failed to serialize HookInput for agent hook: {}", e);
            return HookAction::Allow;
        }
    };

    let prompt = format!(
        "{}\n\nInput:\n```json\n{}\n```\n\nRespond with a JSON object describing the hook action.",
        prompt_template, input_json
    );

    let result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        let llm = llm_factory();
        let mut state = AgentState::new(cwd);

        let agent = ReActAgent::new(llm).max_iterations(max_turns);

        let output = agent
            .execute(AgentInput::text(&prompt), &mut state, None)
            .await?;
        Ok::<_, rust_create_agent::error::AgentError>((output, state.messages().to_vec()))
    })
    .await;

    match result {
        Ok(Ok((_output, messages))) => extract_structured_output(&messages),
        Ok(Err(e)) => {
            tracing::warn!("Agent hook execution failed: {}", e);
            HookAction::Allow
        }
        Err(_) => {
            tracing::warn!("Agent hook timed out after {}s", timeout_secs);
            HookAction::Allow
        }
    }
}

/// Sanitize header value: remove CRLF sequences and expand whitelisted env vars.
///
/// CRLF injection protection: strips \r and \n from header values.
/// Env var expansion: only vars in `allowed_env_vars` set are expanded.
fn sanitize_header_value(value: &str, allowed_env_vars: &HashSet<String>) -> String {
    // First, strip CRLF to prevent injection
    let sanitized = value.replace(['\r', '\n'], "");

    // Expand whitelisted env vars (simple ${VAR} and $VAR patterns)
    let mut result = sanitized;
    for var_name in allowed_env_vars {
        let pattern1 = format!("${{{}}}", var_name);
        let pattern2 = format!("${}", var_name);
        if let Ok(val) = std::env::var(var_name) {
            result = result.replace(&pattern1, &val);
            result = result.replace(&pattern2, &val);
        }
    }

    result
}

/// Extract structured hook output from agent messages.
///
/// Looks through Tool messages for structured output and parses it.
/// Falls back to the last AI message text if no structured output is found.
fn extract_structured_output(messages: &[BaseMessage]) -> HookAction {
    // Look for Tool message results in reverse order (most recent first)
    for msg in messages.iter().rev() {
        if let BaseMessage::Tool { content, .. } = msg {
            let text = content.text_content();
            let action = parse_command_hook_output(&text);
            if !matches!(action, HookAction::Allow) {
                return action;
            }
        }
    }

    // Fallback: check last AI message for JSON
    for msg in messages.iter().rev() {
        if let BaseMessage::Ai { content, .. } = msg {
            let text = content.text_content();
            if text.trim().starts_with('{') {
                return parse_command_hook_output(&text);
            }
        }
    }

    HookAction::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::types::HookEvent;
    use rust_create_agent::messages::MessageId;
    use std::path::PathBuf;

    fn make_registered() -> RegisteredHook {
        RegisteredHook {
            hook: serde_json::from_str(r#"{"type":"command","command":"echo"}"#).unwrap(),
            event: HookEvent::PreToolUse,
            matcher: None,
            plugin_name: "test-plugin".to_string(),
            plugin_id: "test-id".to_string(),
            plugin_root: PathBuf::from("/tmp/test-plugin"),
            plugin_data_dir: PathBuf::from("/tmp/test-plugin-data"),
            plugin_options: std::collections::HashMap::new(),
        }
    }

    fn make_hook_input() -> HookInput {
        HookInput::session_start(
            "sess-1",
            "/tmp/transcript.json",
            "/project",
            "startup",
            "opus",
        )
    }

    fn make_command_hook(command: &str) -> HookType {
        serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": command
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn test_command_hook_echo_plain_text() {
        let hook = make_command_hook("cat");
        let input = make_hook_input();
        let registered = make_registered();
        let action = execute_command_hook(&hook, &input, &registered).await;
        assert!(matches!(action, HookAction::Allow));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_command_hook_exit_code_2_blocks() {
        let hook = make_command_hook("exit 2");
        let input = make_hook_input();
        let registered = make_registered();
        let action = execute_command_hook(&hook, &input, &registered).await;
        assert!(matches!(action, HookAction::Block { .. }));
    }

    #[tokio::test]
    async fn test_command_hook_exit_code_1_allows() {
        let hook = make_command_hook("echo 'error msg' >&2 && exit 1");
        let input = make_hook_input();
        let registered = make_registered();
        let action = execute_command_hook(&hook, &input, &registered).await;
        assert!(matches!(action, HookAction::Allow));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_command_hook_json_output_continue_false() {
        let hook = make_command_hook(r#"echo '{"continue":false,"stopReason":"test stop"}'"#);
        let input = make_hook_input();
        let registered = make_registered();
        let action = execute_command_hook(&hook, &input, &registered).await;
        assert!(matches!(
            action,
            HookAction::PreventContinuation {
                stop_reason: Some(ref s)
            } if s == "test stop"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_command_hook_json_output_block() {
        let hook = make_command_hook(r#"echo '{"decision":"block","reason":"not allowed"}'"#);
        let input = make_hook_input();
        let registered = make_registered();
        let action = execute_command_hook(&hook, &input, &registered).await;
        assert!(matches!(
            action,
            HookAction::Block {
                reason: ref r
            } if r == "not allowed"
        ));
    }

    #[tokio::test]
    async fn test_command_hook_timeout() {
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "sleep 10",
            "timeout": 1
        }))
        .unwrap();
        let input = make_hook_input();
        let registered = make_registered();
        let action = execute_command_hook(&hook, &input, &registered).await;
        assert!(matches!(action, HookAction::Allow));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_command_hook_exit_code_2_with_stdout_reason() {
        let hook = make_command_hook("echo 'custom block reason' && exit 2");
        let input = make_hook_input();
        let registered = make_registered();
        let action = execute_command_hook(&hook, &input, &registered).await;
        assert!(matches!(
            action,
            HookAction::Block {
                reason: ref r
            } if r == "custom block reason"
        ));
    }

    #[tokio::test]
    async fn test_command_hook_plugin_options_env() {
        let mut registered = make_registered();
        registered
            .plugin_options
            .insert("api_key".to_string(), serde_json::json!("sk-test-123"));

        let hook = make_command_hook("echo $CLAUDE_PLUGIN_OPTION_API_KEY");
        let input = make_hook_input();
        let action = execute_command_hook(&hook, &input, &registered).await;
        assert!(matches!(action, HookAction::Allow));
    }

    // === sanitize_header_value tests ===

    #[test]
    fn test_sanitize_crlf_injection() {
        let allowed: HashSet<String> = HashSet::new();
        let result = sanitize_header_value("value\r\nX-Injected: evil", &allowed);
        assert_eq!(result, "valueX-Injected: evil");
    }

    #[test]
    fn test_sanitize_lf_only() {
        let allowed: HashSet<String> = HashSet::new();
        let result = sanitize_header_value("value\nX-Injected: evil", &allowed);
        assert_eq!(result, "valueX-Injected: evil");
    }

    #[test]
    fn test_sanitize_cr_only() {
        let allowed: HashSet<String> = HashSet::new();
        let result = sanitize_header_value("value\rX-Injected: evil", &allowed);
        assert_eq!(result, "valueX-Injected: evil");
    }

    #[test]
    fn test_sanitize_env_var_expansion_allowed() {
        std::env::set_var("TEST_SANITIZE_HOOK_VAR", "secret-value");
        let allowed: HashSet<String> = ["TEST_SANITIZE_HOOK_VAR".to_string()].into_iter().collect();
        let result = sanitize_header_value("Bearer ${TEST_SANITIZE_HOOK_VAR}", &allowed);
        assert_eq!(result, "Bearer secret-value");
        std::env::remove_var("TEST_SANITIZE_HOOK_VAR");
    }

    #[test]
    fn test_sanitize_env_var_expansion_not_allowed() {
        let allowed: HashSet<String> = HashSet::new();
        let result = sanitize_header_value("Bearer ${SECRET_KEY}", &allowed);
        assert_eq!(result, "Bearer ${SECRET_KEY}");
    }

    #[test]
    fn test_sanitize_env_var_brace_expansion() {
        std::env::set_var("TEST_SANITIZE_HOOK_BRACE", "expanded");
        let allowed: HashSet<String> = ["TEST_SANITIZE_HOOK_BRACE".to_string()]
            .into_iter()
            .collect();
        let result = sanitize_header_value("token-${TEST_SANITIZE_HOOK_BRACE}", &allowed);
        assert_eq!(result, "token-expanded");
        std::env::remove_var("TEST_SANITIZE_HOOK_BRACE");
    }

    // === extract_structured_output tests ===

    #[test]
    fn test_extract_empty_messages() {
        let action = extract_structured_output(&[]);
        assert!(matches!(action, HookAction::Allow));
    }

    #[test]
    fn test_extract_no_tool_messages() {
        let messages = vec![BaseMessage::system("no tools here")];
        let action = extract_structured_output(&messages);
        assert!(matches!(action, HookAction::Allow));
    }

    #[test]
    fn test_extract_ai_message_json() {
        use rust_create_agent::messages::MessageContent;

        let messages = vec![BaseMessage::Ai {
            id: MessageId::new(),
            content: MessageContent::text(r#"{"decision":"block","reason":"ai says no"}"#),
            tool_calls: vec![],
        }];
        let action = extract_structured_output(&messages);
        assert!(matches!(
            action,
            HookAction::Block {
                reason: ref r
            } if r == "ai says no"
        ));
    }

    #[test]
    fn test_extract_ai_message_plain_text() {
        use rust_create_agent::messages::MessageContent;

        let messages = vec![BaseMessage::Ai {
            id: MessageId::new(),
            content: MessageContent::text("just some text"),
            tool_calls: vec![],
        }];
        let action = extract_structured_output(&messages);
        assert!(matches!(action, HookAction::Allow));
    }

    #[test]
    fn test_extract_tool_message_with_json() {
        use rust_create_agent::messages::MessageContent;

        let messages = vec![BaseMessage::Tool {
            id: MessageId::new(),
            tool_call_id: "tc-1".into(),
            content: MessageContent::text(r#"{"continue":false,"stopReason":"agent stop"}"#),
            is_error: false,
        }];
        let action = extract_structured_output(&messages);
        assert!(matches!(
            action,
            HookAction::PreventContinuation {
                stop_reason: Some(ref s)
            } if s == "agent stop"
        ));
    }

    // === HTTP hook tests (no mock server, just SSRF/blocking logic) ===

    #[tokio::test]
    async fn test_http_hook_ssrf_blocked() {
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "http",
            "url": "http://192.168.1.1/hook",
            "timeout": 5
        }))
        .unwrap();
        let input = make_hook_input();
        let action = execute_http_hook(&hook, &input).await;
        assert!(matches!(action, HookAction::Block { .. }));
    }

    #[tokio::test]
    async fn test_http_hook_invalid_url() {
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "http",
            "url": "not-a-valid-url",
            "timeout": 5
        }))
        .unwrap();
        let input = make_hook_input();
        let action = execute_http_hook(&hook, &input).await;
        assert!(matches!(action, HookAction::Block { .. }));
    }

    // === Wrong hook type dispatch tests ===

    #[tokio::test]
    async fn test_command_hook_wrong_type_returns_allow() {
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "http",
            "url": "http://example.com"
        }))
        .unwrap();
        let input = make_hook_input();
        let registered = make_registered();
        let action = execute_command_hook(&hook, &input, &registered).await;
        assert!(matches!(action, HookAction::Allow));
    }

    #[tokio::test]
    async fn test_prompt_hook_wrong_type_returns_allow() {
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "echo test"
        }))
        .unwrap();
        let input = make_hook_input();
        let llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
            Arc::new(|| unimplemented!());
        let action = execute_prompt_hook(&hook, &input, &llm_factory).await;
        assert!(matches!(action, HookAction::Allow));
    }

    #[tokio::test]
    async fn test_http_hook_wrong_type_returns_allow() {
        let hook = make_command_hook("echo test");
        let input = make_hook_input();
        let action = execute_http_hook(&hook, &input).await;
        assert!(matches!(action, HookAction::Allow));
    }

    #[tokio::test]
    async fn test_agent_hook_wrong_type_returns_allow() {
        let hook = make_command_hook("echo test");
        let input = make_hook_input();
        let llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
            Arc::new(|| unimplemented!());
        let action = execute_agent_hook(&hook, &input, &llm_factory, "/tmp").await;
        assert!(matches!(action, HookAction::Allow));
    }
}
