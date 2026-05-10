use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};

use rust_create_agent::agent::react::ReactLLM;
use rust_create_agent::agent::react::{AgentOutput, ToolCall, ToolResult};
use rust_create_agent::agent::state::State;
use rust_create_agent::error::{AgentError, AgentResult};
use rust_create_agent::messages::BaseMessage;
use rust_create_agent::middleware::Middleware;

use crate::hooks::executor::{
    execute_agent_hook, execute_command_hook, execute_http_hook, execute_prompt_hook,
};
use crate::hooks::matcher::{matches_if_condition, matches_matcher};
use crate::hooks::types::{HookAction, HookEvent, HookInput, HookType, RegisteredHook};

/// Plugin hook middleware — fires registered hooks at lifecycle events.
pub struct HookMiddleware {
    hooks: Arc<RwLock<HashMap<HookEvent, Vec<RegisteredHook>>>>,
    llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
    cwd: String,
    session_id: String,
    transcript_path: String,
    permission_mode: String,
    current_model: String,
    once_fired: Arc<Mutex<HashSet<String>>>,
    /// Whether this is the first message of a new session (triggers SessionStart).
    is_session_start: bool,
    /// 判断工具是否需要用户审批。用于 PermissionRequest hook 门控。
    /// 默认使用 [`crate::hitl::default_requires_approval`]，
    /// 可通过 `with_requires_approval` 覆盖。
    requires_approval: fn(&str) -> bool,
}

impl HookMiddleware {
    pub fn new(
        registered_hooks: Vec<RegisteredHook>,
        llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
        transcript_path: impl Into<String>,
        permission_mode: impl Into<String>,
        current_model: impl Into<String>,
    ) -> Self {
        Self::with_session_start(
            registered_hooks,
            llm_factory,
            cwd,
            session_id,
            transcript_path,
            permission_mode,
            current_model,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_session_start(
        registered_hooks: Vec<RegisteredHook>,
        llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>,
        cwd: impl Into<String>,
        session_id: impl Into<String>,
        transcript_path: impl Into<String>,
        permission_mode: impl Into<String>,
        current_model: impl Into<String>,
        is_session_start: bool,
    ) -> Self {
        let mut map: HashMap<HookEvent, Vec<RegisteredHook>> = HashMap::new();
        for hook in registered_hooks {
            map.entry(hook.event.clone()).or_default().push(hook);
        }
        let event_count = map.len();
        let total_hooks: usize = map.values().map(|v| v.len()).sum();
        tracing::info!(
            total_hooks,
            event_count,
            is_session_start,
            "HookMiddleware created with registered hooks"
        );
        Self {
            hooks: Arc::new(RwLock::new(map)),
            llm_factory,
            cwd: cwd.into(),
            session_id: session_id.into(),
            transcript_path: transcript_path.into(),
            permission_mode: permission_mode.into(),
            current_model: current_model.into(),
            once_fired: Arc::new(Mutex::new(HashSet::new())),
            is_session_start,
            requires_approval: crate::hitl::default_requires_approval,
        }
    }

    // -----------------------------------------------------------------------
    // fire_event — core dispatch loop
    // -----------------------------------------------------------------------

    async fn fire_event(
        &self,
        event: HookEvent,
        input: &HookInput,
        tool_name: Option<&str>,
        tool_input: Option<&serde_json::Value>,
    ) -> HookAction {
        // 确保 hook_event_name 与实际触发的事件一致。
        //
        // 调用方可能在 before_tool 中复用同一个 HookInput 连续触发多个事件
        // （PreToolUse → PermissionRequest → Notification），而 HookInput::tool_call()
        // 构造函数硬编码 hook_event_name = PreToolUse。若不修正，PermissionRequest hook
        // 脚本从 stdin 读到的 hook_event_name 会是 "PreToolUse" 而非 "PermissionRequest"。
        let input = if input.hook_event_name != event {
            let mut corrected = input.clone();
            corrected.hook_event_name = event.clone();
            corrected
        } else {
            input.clone()
        };

        let hooks = {
            let map = self.hooks.read();
            match map.get(&event) {
                Some(h) => {
                    tracing::debug!(
                        event = ?event,
                        count = h.len(),
                        "HookMiddleware: found hooks for event"
                    );
                    h.clone()
                }
                None => {
                    return HookAction::Allow;
                }
            }
        };

        if hooks.is_empty() {
            return HookAction::Allow;
        }

        let mut final_action = HookAction::Allow;

        for registered in &hooks {
            // once check
            if Self::is_once_hook(&registered.hook) && self.was_once_fired(registered) {
                continue;
            }

            // matcher check
            if let Some(name) = tool_name {
                let matcher_str = registered.matcher.as_deref().unwrap_or_else(|| {
                    registered
                        .hook
                        .get_matcher()
                        .map(|s| s.as_str())
                        .unwrap_or("*")
                });
                if !matches_matcher(matcher_str, name) {
                    continue;
                }
            }

            // if condition check
            if let Some(condition) = registered.hook.get_condition() {
                if let (Some(name), Some(inp)) = (tool_name, tool_input) {
                    if !matches_if_condition(condition, name, inp) {
                        continue;
                    }
                }
            }

            // Execute hook (async hooks are spawned in background, result ignored)
            if let Some(ref msg) = registered.hook.get_status_message() {
                tracing::info!(
                    plugin = %registered.plugin_name,
                    event = ?event,
                    "Hook status: {}",
                    msg
                );
            }
            let action = if registered.hook.is_async() {
                // Fire-and-forget: spawn in background, return Allow immediately
                let hook = registered.hook.clone();
                let owned_input = input.clone();
                let registered = registered.clone();
                tokio::spawn(async move {
                    let _ = match &hook {
                        HookType::Command { .. } => {
                            execute_command_hook(&hook, &owned_input, &registered).await
                        }
                        HookType::Http { .. } => execute_http_hook(&hook, &owned_input).await,
                        // Prompt/Agent hooks need LLM factory which can't be cloned into spawn;
                        // async only applies to Command per schema definition.
                        _ => HookAction::Allow,
                    };
                });
                HookAction::Allow
            } else {
                match &registered.hook {
                    HookType::Command { .. } => {
                        execute_command_hook(&registered.hook, &input, registered).await
                    }
                    HookType::Prompt { .. } => {
                        execute_prompt_hook(&registered.hook, &input, &self.llm_factory).await
                    }
                    HookType::Http { .. } => execute_http_hook(&registered.hook, &input).await,
                    HookType::Agent { .. } => {
                        execute_agent_hook(&registered.hook, &input, &self.llm_factory, &self.cwd)
                            .await
                    }
                }
            };

            // once mark
            if Self::is_once_hook(&registered.hook) {
                self.mark_once_fired(registered);
            }

            // Short-circuit on Block / PreventContinuation
            match &action {
                HookAction::Block { .. } | HookAction::PreventContinuation { .. } => return action,
                HookAction::ModifyInput { new_input } => {
                    final_action = HookAction::ModifyInput {
                        new_input: new_input.clone(),
                    };
                }
                _ => {}
            }
        }

        final_action
    }

    // -----------------------------------------------------------------------
    // Helper methods
    // -----------------------------------------------------------------------

    fn is_once_hook(hook: &HookType) -> bool {
        hook.is_once()
    }

    fn once_key(registered: &RegisteredHook) -> String {
        format!(
            "{}:{}:{:?}",
            registered.plugin_id,
            serde_json::to_string(&registered.hook).unwrap_or_default(),
            registered.event
        )
    }

    fn was_once_fired(&self, registered: &RegisteredHook) -> bool {
        let key = Self::once_key(registered);
        self.once_fired.lock().contains(&key)
    }

    fn mark_once_fired(&self, registered: &RegisteredHook) {
        let key = Self::once_key(registered);
        self.once_fired.lock().insert(key);
    }
}

#[async_trait]
impl<S: State> Middleware<S> for HookMiddleware {
    fn name(&self) -> &str {
        "HookMiddleware"
    }

    async fn before_agent(&self, state: &mut S) -> AgentResult<()> {
        // Extract the latest human message as prompt text
        let prompt = state
            .messages()
            .iter()
            .rev()
            .find(|m| matches!(m, BaseMessage::Human { .. }))
            .map(|m| m.content())
            .unwrap_or_default();

        // SessionStart: only when is_session_start is true (first message of a new session)
        if self.is_session_start {
            let input = HookInput::session_start(
                &self.session_id,
                &self.transcript_path,
                &self.cwd,
                "startup",
                &self.current_model,
            );
            let action = self
                .fire_event(HookEvent::SessionStart, &input, None, None)
                .await;
            match &action {
                HookAction::Block { reason } => {
                    return Err(AgentError::ToolRejected {
                        tool: "SessionStart".to_string(),
                        reason: reason.clone(),
                    });
                }
                HookAction::PreventContinuation { stop_reason } => {
                    let reason = stop_reason
                        .clone()
                        .unwrap_or_else(|| "SessionStart hook prevented continuation".to_string());
                    return Err(AgentError::ToolRejected {
                        tool: "SessionStart".to_string(),
                        reason,
                    });
                }
                HookAction::SystemMessage { message } => {
                    tracing::info!("SessionStart hook system message: {}", message);
                }
                HookAction::AdditionalContext { context } => {
                    tracing::info!("SessionStart hook additional context: {}", context);
                }
                HookAction::InitialUserMessage { message } => {
                    tracing::info!("SessionStart hook initial user message: {}", message);
                }
                _ => {}
            }
        }

        // UserPromptSubmit: on every user prompt
        let input = HookInput::user_prompt_submit(
            &self.session_id,
            &self.transcript_path,
            &self.cwd,
            &prompt,
        );
        let action = self
            .fire_event(HookEvent::UserPromptSubmit, &input, None, None)
            .await;

        // Handle UserPromptSubmit actions
        match &action {
            HookAction::Block { reason } => {
                return Err(AgentError::ToolRejected {
                    tool: "UserPromptSubmit".to_string(),
                    reason: reason.clone(),
                });
            }
            HookAction::PreventContinuation { stop_reason } => {
                let reason = stop_reason
                    .clone()
                    .unwrap_or_else(|| "Hook prevented continuation".to_string());
                return Err(AgentError::ToolRejected {
                    tool: "UserPromptSubmit".to_string(),
                    reason,
                });
            }
            _ => {}
        }

        Ok(())
    }

    async fn before_tool(&self, _state: &mut S, tool_call: &ToolCall) -> AgentResult<ToolCall> {
        let input = HookInput::tool_call(
            &self.session_id,
            &self.transcript_path,
            &self.cwd,
            &self.permission_mode,
            &tool_call.name,
            &tool_call.input,
            &tool_call.id,
        );

        // Fire PreToolUse
        let action = self
            .fire_event(
                HookEvent::PreToolUse,
                &input,
                Some(&tool_call.name),
                Some(&tool_call.input),
            )
            .await;

        match &action {
            HookAction::Block { reason } => {
                return Err(AgentError::ToolRejected {
                    tool: tool_call.name.clone(),
                    reason: reason.clone(),
                });
            }
            HookAction::PreventContinuation { stop_reason } => {
                let reason = stop_reason
                    .clone()
                    .unwrap_or_else(|| "Hook prevented continuation".to_string());
                return Err(AgentError::ToolRejected {
                    tool: tool_call.name.clone(),
                    reason,
                });
            }
            HookAction::ModifyInput { new_input } => {
                return Ok(ToolCall {
                    id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    input: new_input.clone(),
                });
            }
            _ => {}
        }

        // PermissionRequest 门控：仅对敏感工具触发。
        //
        // 使用 hitl::default_requires_approval 判断工具是否需要审批（Bash/Write/Edit/Agent/
        // mcp__*/WebFetch/WebSearch 等）。非敏感工具（Read/Glob/Grep 等）不触发。
        //
        // 不检查 permission_mode（YOLO/审批）：hook 始终触发以便观察/日志，HITL 弹窗是否显示
        // 由 HITL 中间件独立决定。
        let is_sensitive = (self.requires_approval)(&tool_call.name);

        if is_sensitive {
            let action = self
                .fire_event(
                    HookEvent::PermissionRequest,
                    &input,
                    Some(&tool_call.name),
                    Some(&tool_call.input),
                )
                .await;

            // Fire Notification (agent is waiting for user permission)
            self.fire_event(
                HookEvent::Notification,
                &input,
                Some(&tool_call.name),
                Some(&tool_call.input),
            )
            .await;

            match &action {
                HookAction::Block { reason } => {
                    return Err(AgentError::ToolRejected {
                        tool: tool_call.name.clone(),
                        reason: reason.clone(),
                    });
                }
                HookAction::PreventContinuation { stop_reason } => {
                    let reason = stop_reason
                        .clone()
                        .unwrap_or_else(|| "Hook prevented continuation".to_string());
                    return Err(AgentError::ToolRejected {
                        tool: tool_call.name.clone(),
                        reason,
                    });
                }
                HookAction::ModifyInput { new_input } => {
                    return Ok(ToolCall {
                        id: tool_call.id.clone(),
                        name: tool_call.name.clone(),
                        input: new_input.clone(),
                    });
                }
                _ => {}
            }
        }

        Ok(tool_call.clone())
    }

    async fn after_tool(
        &self,
        _state: &mut S,
        tool_call: &ToolCall,
        result: &ToolResult,
    ) -> AgentResult<()> {
        let event = if result.is_error {
            HookEvent::PostToolUseFailure
        } else {
            HookEvent::PostToolUse
        };

        let input = HookInput::tool_result(
            &self.session_id,
            &self.transcript_path,
            &self.cwd,
            &self.permission_mode,
            &tool_call.name,
            &tool_call.input,
            &serde_json::json!(result.output),
            result.is_error,
        );

        let _action = self
            .fire_event(event, &input, Some(&tool_call.name), Some(&tool_call.input))
            .await;

        Ok(())
    }

    async fn after_agent(&self, _state: &mut S, output: &AgentOutput) -> AgentResult<AgentOutput> {
        // 构造 Stop hook 的 HookInput。
        // subagent_result 携带 agent 最终输出（截断到 500 字符），
        // source 携带 stop_reason（若存在）标识结束原因。
        let input = HookInput {
            session_id: self.session_id.clone(),
            transcript_path: self.transcript_path.clone(),
            cwd: self.cwd.clone(),
            permission_mode: Some(self.permission_mode.clone()),
            agent_id: None,
            agent_type: None,
            hook_event_name: HookEvent::Stop,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: None,
            source: output
                .stop_reason
                .as_deref()
                .map(|_| "agent_complete".to_string()),
            model: Some(self.current_model.clone()),
            subagent_name: None,
            subagent_result: Some(output.text.chars().take(500).collect::<String>()),
            message_count: None,
        };

        let _action = self.fire_event(HookEvent::Stop, &input, None, None).await;

        // Fire Notification (agent done, waiting for user input)
        self.fire_event(HookEvent::Notification, &input, None, None)
            .await;

        Ok(output.clone())
    }

    async fn on_error(
        &self,
        _state: &mut S,
        error: &rust_create_agent::error::AgentError,
    ) -> AgentResult<()> {
        // 当 agent 因错误退出时触发 StopFailure hook。
        // 这覆盖了 Interrupted、MaxIterationsExceeded、LLM 调用失败等场景，
        // 这些路径不经过 after_agent（直接返回 Err），因此需要在此处单独触发。
        let error_description = format!("{:?}", error);
        let input = HookInput {
            session_id: self.session_id.clone(),
            transcript_path: self.transcript_path.clone(),
            cwd: self.cwd.clone(),
            permission_mode: Some(self.permission_mode.clone()),
            agent_id: None,
            agent_type: None,
            hook_event_name: HookEvent::StopFailure,
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: Some(serde_json::json!(error_description)),
            prompt: None,
            source: None,
            model: Some(self.current_model.clone()),
            subagent_name: None,
            subagent_result: None,
            message_count: None,
        };

        self.fire_event(HookEvent::StopFailure, &input, None, None)
            .await;

        Ok(())
    }
}

/// Fire standalone lifecycle hooks outside of the middleware lifecycle.
///
/// Used by the TUI layer for events that occur outside the agent ReAct loop:
/// - `SessionEnd`: when `/clear` resets the session
/// - `PreCompact` / `PostCompact`: before/after context compaction
/// - `Notification`: when agent needs user attention (e.g. AskUserQuestion)
///
/// The HookMiddleware instance is owned by the agent task and not accessible
/// from these code paths, so we dispatch hooks directly.
pub async fn fire_standalone_lifecycle_hooks(
    registered_hooks: &[RegisteredHook],
    event: HookEvent,
    cwd: &str,
    session_id: &str,
    transcript_path: &str,
    current_model: &str,
    message_count: Option<usize>,
) {
    // Filter hooks matching the event
    let matching: Vec<&RegisteredHook> = registered_hooks
        .iter()
        .filter(|h| h.event == event)
        .collect();

    if matching.is_empty() {
        return;
    }

    let input = match &event {
        HookEvent::SessionEnd => HookInput {
            session_id: session_id.to_string(),
            transcript_path: transcript_path.to_string(),
            cwd: cwd.to_string(),
            permission_mode: None,
            agent_id: None,
            agent_type: None,
            hook_event_name: event.clone(),
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: None,
            source: None,
            model: Some(current_model.to_string()),
            subagent_name: None,
            subagent_result: None,
            message_count: None,
        },
        HookEvent::PreCompact | HookEvent::PostCompact => HookInput::compact(
            session_id,
            transcript_path,
            cwd,
            event.clone(),
            message_count.unwrap_or(0),
        ),
        HookEvent::Notification => HookInput {
            session_id: session_id.to_string(),
            transcript_path: transcript_path.to_string(),
            cwd: cwd.to_string(),
            permission_mode: None,
            agent_id: None,
            agent_type: None,
            hook_event_name: event.clone(),
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_output: None,
            prompt: None,
            source: None,
            model: Some(current_model.to_string()),
            subagent_name: None,
            subagent_result: None,
            message_count: None,
        },
        _ => return,
    };

    for registered in matching {
        if let Some(ref msg) = registered.hook.get_status_message() {
            tracing::info!(
                plugin = %registered.plugin_name,
                event = ?event,
                "Hook status: {}",
                msg
            );
        }

        if registered.hook.is_async() {
            // Fire-and-forget async hook
            let hook = registered.hook.clone();
            let input = input.clone();
            let registered = registered.clone();
            tokio::spawn(async move {
                let _ = match &hook {
                    HookType::Command { .. } => {
                        execute_command_hook(&hook, &input, &registered).await
                    }
                    HookType::Http { .. } => execute_http_hook(&hook, &input).await,
                    _ => HookAction::Allow,
                };
            });
            continue;
        }

        let _action = match &registered.hook {
            HookType::Command { .. } => {
                execute_command_hook(&registered.hook, &input, registered).await
            }
            HookType::Prompt { .. } => {
                // No LLM factory available in standalone context; skip
                HookAction::Allow
            }
            HookType::Http { .. } => execute_http_hook(&registered.hook, &input).await,
            HookType::Agent { .. } => {
                // No LLM factory available in standalone context; skip
                HookAction::Allow
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_registered(event: HookEvent, hook: HookType) -> RegisteredHook {
        RegisteredHook {
            hook,
            event,
            matcher: None,
            plugin_name: "test-plugin".to_string(),
            plugin_id: "test-plugin-id".to_string(),
            plugin_root: PathBuf::from("/tmp/test-plugin"),
            plugin_data_dir: PathBuf::from("/tmp/test-plugin-data"),
            plugin_options: HashMap::new(),
        }
    }

    fn make_llm_factory() -> Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> {
        Arc::new(|| unimplemented!("no LLM needed in unit tests"))
    }

    fn make_middleware(hooks: Vec<RegisteredHook>) -> HookMiddleware {
        HookMiddleware::new(
            hooks,
            make_llm_factory(),
            "/test-cwd",
            "test-session",
            "/test/transcript.json",
            "yolo",
            "opus",
        )
    }

    fn make_middleware_hitl(hooks: Vec<RegisteredHook>) -> HookMiddleware {
        HookMiddleware::new(
            hooks,
            make_llm_factory(),
            "/test-cwd",
            "test-session",
            "/test/transcript.json",
            "hitl",
            "opus",
        )
    }

    #[tokio::test]
    async fn test_fire_event_no_hooks() {
        let mw = make_middleware(vec![]);
        let input = HookInput::session_start("s", "/t", "/c", "startup", "opus");
        let action = mw
            .fire_event(HookEvent::SessionStart, &input, None, None)
            .await;
        assert!(matches!(action, HookAction::Allow));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_fire_event_once_semantic() {
        // once hook should fire only once
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "exit 2",
            "once": true
        }))
        .unwrap();

        let registered = make_registered(HookEvent::PreToolUse, hook);
        let mw = make_middleware(vec![registered]);

        let input = HookInput::tool_call(
            "s",
            "/t",
            "/c",
            "yolo",
            "Bash",
            &serde_json::json!({"command": "ls"}),
            "c1",
        );

        // First call → Block (exit code 2)
        let action = mw
            .fire_event(
                HookEvent::PreToolUse,
                &input,
                Some("Bash"),
                Some(&serde_json::json!({"command": "ls"})),
            )
            .await;
        assert!(matches!(action, HookAction::Block { .. }));

        // Second call → Allow (once already fired)
        let action = mw
            .fire_event(
                HookEvent::PreToolUse,
                &input,
                Some("Bash"),
                Some(&serde_json::json!({"command": "ls"})),
            )
            .await;
        assert!(matches!(action, HookAction::Allow));
    }

    #[tokio::test]
    async fn test_fire_event_matcher_filter() {
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "exit 2",
            "matcher": "Write"
        }))
        .unwrap();

        let registered = make_registered(HookEvent::PreToolUse, hook);
        let mw = make_middleware(vec![registered]);

        let input = HookInput::tool_call(
            "s",
            "/t",
            "/c",
            "yolo",
            "Bash",
            &serde_json::json!({"command": "ls"}),
            "c1",
        );

        // Matcher is "Write" but tool is "Bash" → skip → Allow
        let action = mw
            .fire_event(
                HookEvent::PreToolUse,
                &input,
                Some("Bash"),
                Some(&serde_json::json!({"command": "ls"})),
            )
            .await;
        assert!(matches!(action, HookAction::Allow));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_fire_event_block_short_circuit() {
        let hook1: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "exit 2"
        }))
        .unwrap();
        let hook2: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "echo should-not-run"
        }))
        .unwrap();

        let r1 = make_registered(HookEvent::PreToolUse, hook1);
        let r2 = make_registered(HookEvent::PreToolUse, hook2);
        let mw = make_middleware(vec![r1, r2]);

        let input = HookInput::tool_call(
            "s",
            "/t",
            "/c",
            "yolo",
            "Bash",
            &serde_json::json!({"command": "ls"}),
            "c1",
        );

        // First hook blocks → short-circuit, second never runs
        let action = mw
            .fire_event(
                HookEvent::PreToolUse,
                &input,
                Some("Bash"),
                Some(&serde_json::json!({"command": "ls"})),
            )
            .await;
        assert!(matches!(action, HookAction::Block { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_before_tool_block() {
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "exit 2"
        }))
        .unwrap();

        let registered = make_registered(HookEvent::PreToolUse, hook);
        let mw = make_middleware(vec![registered]);

        let tool_call = ToolCall::new("c1", "Bash", serde_json::json!({"command": "ls"}));

        let result = mw
            .before_tool(
                &mut rust_create_agent::agent::state::AgentState::new("/test"),
                &tool_call,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            AgentError::ToolRejected { tool, reason } => {
                assert_eq!(tool, "Bash");
                assert!(!reason.is_empty());
            }
            other => panic!("Expected ToolRejected, got: {:?}", other),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_before_tool_modify_input() {
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "echo '{\"hook_specific_output\":{\"hookEventName\":\"PreToolUse\",\"updatedInput\":{\"command\":\"safe-ls\"}}}'"
        }))
        .unwrap();

        let registered = make_registered(HookEvent::PreToolUse, hook);
        let mw = make_middleware(vec![registered]);

        let tool_call = ToolCall::new("c1", "Bash", serde_json::json!({"command": "rm -rf /"}));

        let result = mw
            .before_tool(
                &mut rust_create_agent::agent::state::AgentState::new("/test"),
                &tool_call,
            )
            .await;

        assert!(result.is_ok());
        let modified = result.unwrap();
        assert_eq!(modified.name, "Bash");
        // The command should have been modified
        assert_eq!(modified.input["command"], "safe-ls");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_before_agent_fires_user_prompt_submit() {
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "exit 2"
        }))
        .unwrap();

        let registered = make_registered(HookEvent::UserPromptSubmit, hook);
        let mw = make_middleware(vec![registered]);

        let mut state = rust_create_agent::agent::state::AgentState::new("/test");
        state.add_message(BaseMessage::human("hello world"));

        // UserPromptSubmit hook blocks → should return error
        let result = mw.before_agent(&mut state).await;
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_before_agent_session_start_controlled_by_flag() {
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "exit 2"
        }))
        .unwrap();

        let registered = make_registered(HookEvent::SessionStart, hook);

        // is_session_start=true → SessionStart fires → blocks
        let mw = HookMiddleware::with_session_start(
            vec![registered.clone()],
            make_llm_factory(),
            "/test-cwd",
            "test-session",
            "/test/transcript.json",
            "yolo",
            "opus",
            true,
        );
        let mut state = rust_create_agent::agent::state::AgentState::new("/test");
        state.add_message(BaseMessage::human("first"));
        let result = mw.before_agent(&mut state).await;
        assert!(result.is_err());

        // is_session_start=false → SessionStart skipped → ok
        let mw2 = HookMiddleware::with_session_start(
            vec![registered],
            make_llm_factory(),
            "/test-cwd",
            "test-session",
            "/test/transcript.json",
            "yolo",
            "opus",
            false,
        );
        let mut state2 = rust_create_agent::agent::state::AgentState::new("/test");
        state2.add_message(BaseMessage::human("second"));
        let result = mw2.before_agent(&mut state2).await;
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_before_tool_fires_permission_request() {
        // PermissionRequest hook with exit code 2 → Block
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "exit 2"
        }))
        .unwrap();

        let registered = make_registered(HookEvent::PermissionRequest, hook);
        let mw = make_middleware_hitl(vec![registered]);

        let tool_call = ToolCall::new(
            "c1",
            "Write",
            serde_json::json!({"path": "/tmp/test", "content": "hello"}),
        );

        let result = mw
            .before_tool(
                &mut rust_create_agent::agent::state::AgentState::new("/test"),
                &tool_call,
            )
            .await;

        assert!(result.is_err());
        match result.unwrap_err() {
            AgentError::ToolRejected { tool, reason } => {
                assert_eq!(tool, "Write");
                assert!(!reason.is_empty());
            }
            other => panic!(
                "Expected ToolRejected from PermissionRequest, got: {:?}",
                other
            ),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_before_tools_batch_fires_permission_request() {
        // Verify that the default before_tools_batch (which calls before_tool per call)
        // correctly fires PermissionRequest for sensitive tools in a batch.
        use rust_create_agent::middleware::Middleware;

        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "exit 2"
        }))
        .unwrap();

        let registered = make_registered(HookEvent::PermissionRequest, hook);
        let mw = make_middleware_hitl(vec![registered]);

        let calls = vec![
            ToolCall::new("c1", "Write", serde_json::json!({"path": "/a"})),
            ToolCall::new("c2", "Read", serde_json::json!({"path": "/b"})),
        ];

        let mut state = rust_create_agent::agent::state::AgentState::new("/test");
        let results = mw.before_tools_batch(&mut state, &calls).await;

        assert_eq!(results.len(), 2);
        // Write is sensitive → PermissionRequest fires → rejected
        assert!(
            results[0].is_err(),
            "Write should be rejected by PermissionRequest"
        );
        // Read is NOT sensitive → PermissionRequest skipped → allowed
        assert!(
            results[1].is_ok(),
            "Read should be allowed (not sensitive, no PermissionRequest)"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_before_tool_fires_both_pre_tool_use_and_permission_request() {
        // PreToolUse: allow (exit 0), PermissionRequest: block (exit 2)
        let pre_hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "exit 0"
        }))
        .unwrap();
        let perm_hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": "exit 2"
        }))
        .unwrap();

        let r1 = make_registered(HookEvent::PreToolUse, pre_hook);
        let r2 = make_registered(HookEvent::PermissionRequest, perm_hook);
        let mw = make_middleware_hitl(vec![r1, r2]);

        let tool_call = ToolCall::new("c1", "Bash", serde_json::json!({"command": "ls"}));

        // PreToolUse allows, PermissionRequest blocks
        let result = mw
            .before_tool(
                &mut rust_create_agent::agent::state::AgentState::new("/test"),
                &tool_call,
            )
            .await;
        assert!(
            result.is_err(),
            "PermissionRequest should block the tool call"
        );
    }

    /// End-to-end test: async PermissionRequest hook writes a marker file, verifying it actually fires.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_async_permission_request_hook_actually_fires() {
        let marker_path = "/tmp/perihelion_async_hook_test_marker";
        let _ = std::fs::remove_file(marker_path);

        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": format!("echo fired > {}", marker_path),
            "async": true
        }))
        .unwrap();

        let registered = make_registered(HookEvent::PermissionRequest, hook);
        let mw = make_middleware_hitl(vec![registered]);

        let tool_call = ToolCall::new("c1", "Write", serde_json::json!({"path": "/tmp/test"}));

        // before_tool should return Ok (async hook fires in background, returns Allow)
        let result = mw
            .before_tool(
                &mut rust_create_agent::agent::state::AgentState::new("/test"),
                &tool_call,
            )
            .await;
        assert!(result.is_ok(), "Async hook should return Allow (Ok)");

        // Wait for the spawned task to complete
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Verify the marker file was created by the async hook
        assert!(
            std::path::Path::new(marker_path).exists(),
            "Async hook should have created marker file"
        );
        let content = std::fs::read_to_string(marker_path).unwrap_or_default();
        assert!(
            content.contains("fired"),
            "Marker should contain 'fired', got: {}",
            content
        );

        let _ = std::fs::remove_file(marker_path);
    }

    /// Verify async hook receives correct HookInput with hook_event_name = PermissionRequest
    #[cfg(unix)]
    #[tokio::test]
    async fn test_async_hook_receives_correct_event_name() {
        let marker_path = "/tmp/perihelion_async_hook_event_marker";
        let _ = std::fs::remove_file(marker_path);

        // Hook that writes hook_event_name from stdin JSON to a file
        let marker = marker_path.to_string();
        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": format!("python3 -c \"import json,sys; d=json.load(sys.stdin); open('{}','w').write(d['hook_event_name'])\"", marker),
            "async": true
        }))
        .unwrap();

        let registered = make_registered(HookEvent::PermissionRequest, hook);
        let mw = make_middleware_hitl(vec![registered]);

        let tool_call = ToolCall::new("c1", "Write", serde_json::json!({"path": "/tmp/test"}));

        let _ = mw
            .before_tool(
                &mut rust_create_agent::agent::state::AgentState::new("/test"),
                &tool_call,
            )
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

        assert!(
            std::path::Path::new(marker_path).exists(),
            "Async hook should have created marker file"
        );
        let content = std::fs::read_to_string(marker_path).unwrap_or_default();
        assert_eq!(
            content, "PermissionRequest",
            "hook_event_name should be PermissionRequest, got: {}",
            content
        );

        let _ = std::fs::remove_file(marker_path);
    }

    /// Verify PermissionRequest DOES fire in YOLO mode for sensitive tools,
    /// but the hook returning Allow means the tool proceeds normally.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_permission_request_fires_even_in_yolo_mode() {
        let marker_path = "/tmp/perihelion_yolo_fire_marker";
        let _ = std::fs::remove_file(marker_path);

        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": format!("echo fired > {}", marker_path),
            "async": false
        }))
        .unwrap();

        let registered = make_registered(HookEvent::PermissionRequest, hook);
        let mw = make_middleware(vec![registered]); // "yolo" mode

        let tool_call = ToolCall::new("c1", "Bash", serde_json::json!({"command": "ls"}));
        let result = mw
            .before_tool(
                &mut rust_create_agent::agent::state::AgentState::new("/test"),
                &tool_call,
            )
            .await;

        // Hook fires (exit 0 → Allow), tool proceeds
        assert!(result.is_ok(), "YOLO mode: hook allows, tool proceeds");
        assert!(
            std::path::Path::new(marker_path).exists(),
            "PermissionRequest hook SHOULD fire even in YOLO mode for sensitive tools"
        );
        let _ = std::fs::remove_file(marker_path);
    }

    /// Verify PermissionRequest does NOT fire for non-sensitive tools (Read, Glob, etc.)
    #[tokio::test]
    async fn test_permission_request_skipped_for_non_sensitive_tools() {
        let marker_path = "/tmp/perihelion_nonsensitive_marker";
        let _ = std::fs::remove_file(marker_path);

        let hook: HookType = serde_json::from_value(serde_json::json!({
            "type": "command",
            "command": format!("echo fired > {}", marker_path),
            "async": false
        }))
        .unwrap();

        let registered = make_registered(HookEvent::PermissionRequest, hook);
        let mw = make_middleware_hitl(vec![registered]);

        // Read is NOT in the sensitive tools list
        let tool_call = ToolCall::new("c1", "Read", serde_json::json!({"path": "/tmp/test"}));
        let result = mw
            .before_tool(
                &mut rust_create_agent::agent::state::AgentState::new("/test"),
                &tool_call,
            )
            .await;

        assert!(result.is_ok(), "Read should not trigger PermissionRequest");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(
            !std::path::Path::new(marker_path).exists(),
            "PermissionRequest should NOT fire for non-sensitive tools"
        );
    }
}
