// ── Live code retained after Task 6 ACP separation ──
// - map_executor_event: used by handle_acp_notification bridge (agent_ops.rs)
// - compact_task: used by start_compact / auto-compact flow (thread_ops.rs, command/compact.rs)

use tokio::sync::mpsc;
use tracing::warn;

pub use super::provider::LlmProvider;
use super::AgentEvent;
use peri_agent::agent::events::AgentEvent as ExecutorEvent;
use peri_agent::agent::AgentCancellationToken;

// ─── 辅助函数 ─────────────────────────────────────────────────────────────────

use super::tool_display::{format_tool_args, format_tool_name, truncate};

/// 将 ExecutorEvent 映射为 TUI AgentEvent；不需转发的内部事件返回 None
pub(crate) fn map_executor_event(event: ExecutorEvent, cwd: &str) -> Option<AgentEvent> {
    Some(match event {
        ExecutorEvent::AiReasoning(text) => AgentEvent::AiReasoning(text),
        ExecutorEvent::TextChunk {
            chunk: text,
            source_agent_id,
            ..
        } => AgentEvent::AssistantChunk {
            chunk: text,
            source_agent_id,
        },
        // Agent ToolStart → SubAgentStart（在通用 ToolStart 分支之前）
        ExecutorEvent::ToolStart { name, input, .. } if name == "Agent" => {
            let agent_id = input
                .get("subagent_type")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("fork")
                .to_string();
            let task_preview = input["prompt"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(40)
                .collect();
            let is_background = input["run_in_background"].as_bool().unwrap_or(false);
            AgentEvent::SubAgentStart {
                agent_id,
                task_preview,
                is_background,
            }
        }
        ExecutorEvent::ToolStart {
            tool_call_id,
            name,
            input,
            source_agent_id,
            ..
        } => AgentEvent::ToolStart {
            tool_call_id,
            name: name.clone(),
            display: format_tool_name(&name),
            args: format_tool_args(&name, &input, Some(cwd)).unwrap_or_default(),
            input: input.clone(),
            source_agent_id,
        },
        // ask_user 成功：显示用户的回答
        ExecutorEvent::ToolEnd {
            tool_call_id,
            name,
            output,
            is_error: false,
            source_agent_id,
            ..
        } if name == "AskUserQuestion" => AgentEvent::ToolEnd {
            tool_call_id,
            name,
            output: format!("? → {}", truncate(&output, 60)),
            is_error: false,
            source_agent_id,
        },
        // 工具执行出错
        ExecutorEvent::ToolEnd {
            tool_call_id,
            name,
            output,
            is_error: true,
            source_agent_id,
            ..
        } => AgentEvent::ToolEnd {
            tool_call_id,
            name,
            output: format!("✗ {}", truncate(&output, 60)),
            is_error: true,
            source_agent_id,
        },
        // 无需转发的内部事件（ToolEnd 成功事件需要转发以更新 ToolBlock 内容）
        ExecutorEvent::StateSnapshot(msgs) => AgentEvent::StateSnapshot(msgs),
        ExecutorEvent::StepDone { .. }
        | ExecutorEvent::MessageAdded(_)
        | ExecutorEvent::LlmCallStart { .. } => return None,
        // 成功的 ToolEnd（非 Agent / AskUserQuestion / error）
        ExecutorEvent::ToolEnd {
            tool_call_id,
            name,
            output,
            source_agent_id,
            ..
        } => AgentEvent::ToolEnd {
            tool_call_id,
            name,
            output: truncate(&output, 200),
            is_error: false,
            source_agent_id,
        },
        // 上下文使用警告：映射为 TUI 层事件，由 handle_agent_event 触发 auto-compact
        ExecutorEvent::ContextWarning {
            used_tokens,
            total_tokens,
            percentage,
        } => AgentEvent::ContextWarning {
            used_tokens,
            total_tokens,
            percentage,
        },
        ExecutorEvent::LlmCallEnd {
            usage: Some(usage),
            model,
            ..
        } => AgentEvent::TokenUsageUpdate { usage, model },
        ExecutorEvent::LlmCallEnd { usage: None, .. } => return None,
        ExecutorEvent::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        } => AgentEvent::LlmRetrying {
            attempt,
            max_attempts,
            delay_ms,
            error,
        },
        ExecutorEvent::BackgroundTaskCompleted(result) => AgentEvent::BackgroundTaskCompleted {
            task_id: result.task_id,
            agent_name: result.agent_name,
            success: result.success,
            output: result.output,
            tool_calls_count: result.tool_calls_count,
            duration_ms: result.duration_ms,
        },
        ExecutorEvent::LspDiagnostics {
            errors,
            warnings,
            files_with_errors,
        } => AgentEvent::LspDiagnostics {
            errors,
            warnings,
            files_with_errors,
        },
        // SubAgent 生命周期事件 → 触发 spinner 更新 + 刷新显示
        ExecutorEvent::SubagentStarted { agent_name } => AgentEvent::SubagentLifecycle {
            agent_name,
            started: true,
        },
        ExecutorEvent::SubagentStopped {
            agent_name,
            result,
            is_error,
        } => AgentEvent::SubAgentEnd {
            agent_id: Some(agent_name),
            result,
            is_error,
        },
        ExecutorEvent::CompactStarted => AgentEvent::CompactStarted,
        ExecutorEvent::CompactCompleted {
            summary,
            files,
            skills,
            micro_cleared,
            messages,
        } => AgentEvent::CompactCompleted {
            summary,
            files,
            skills,
            micro_cleared,
            messages,
        },
        ExecutorEvent::CompactError { message } => AgentEvent::CompactError(message),
        ExecutorEvent::SessionEnded => return None,
    })
}

// ─── 上下文压缩任务 ────────────────────────────────────────────────────────────

/// 独立的上下文压缩异步任务：调用核心层 full_compact + re_inject 三阶段流程
#[allow(clippy::too_many_arguments)]
pub async fn compact_task(
    messages: Vec<peri_agent::messages::BaseMessage>,
    model: Box<dyn peri_agent::llm::BaseModel>,
    instructions: String,
    config: peri_agent::agent::CompactConfig,
    cwd: String,
    tx: mpsc::Sender<super::AgentEvent>,
    cancel: AgentCancellationToken,
    registered_hooks: Vec<peri_middlewares::hooks::types::RegisteredHook>,
    session_id: String,
    transcript_path: String,
    provider_name: String,
) {
    use peri_agent::agent::{full_compact, re_inject};
    use peri_middlewares::hooks::middleware::fire_standalone_lifecycle_hooks;
    use peri_middlewares::hooks::types::HookEvent;

    let msg_count = messages.len();

    tracing::info!(msg_count = msg_count, "compact_task: 开始 Full Compact");

    // Fire PreCompact hooks
    fire_standalone_lifecycle_hooks(
        &registered_hooks,
        HookEvent::PreCompact,
        &cwd,
        &session_id,
        &transcript_path,
        &provider_name,
        Some(msg_count),
    )
    .await;

    // full_compact 调用 LLM，支持取消
    let compact_result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            tracing::info!("compact_task: 被用户取消");
            if tx.send(super::AgentEvent::CompactError("已取消".to_string())).await.is_err() {
                warn!("compact_task: failed to send CompactError (channel closed)");
            }
            // Fire PostCompact even on cancel
            fire_standalone_lifecycle_hooks(
                &registered_hooks,
                HookEvent::PostCompact,
                &cwd,
                &session_id,
                &transcript_path,
                &provider_name,
                Some(msg_count),
            )
            .await;
            return;
        }
        result = full_compact(&messages, model.as_ref(), &config, &instructions) => {
            match result {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(error = %e, "compact_task: Full Compact 失败");
                    if tx.send(super::AgentEvent::CompactError(e.to_string())).await.is_err() {
                        warn!("compact_task: failed to send CompactError (channel closed)");
                    }
                    // Fire PostCompact even on failure
                    fire_standalone_lifecycle_hooks(
                        &registered_hooks,
                        HookEvent::PostCompact,
                        &cwd,
                        &session_id,
                        &transcript_path,
                        &provider_name,
                        Some(msg_count),
                    )
                    .await;
                    return;
                }
            }
        }
    };

    // 取消检查：re_inject 之前
    if cancel.is_cancelled() {
        tracing::info!("compact_task: re_inject 前被取消");
        if tx
            .send(super::AgentEvent::CompactError("已取消".to_string()))
            .await
            .is_err()
        {
            warn!("compact_task: failed to send CompactError on re_inject cancel (channel closed)");
        }
        fire_standalone_lifecycle_hooks(
            &registered_hooks,
            HookEvent::PostCompact,
            &cwd,
            &session_id,
            &transcript_path,
            &provider_name,
            Some(msg_count),
        )
        .await;
        return;
    }

    tracing::info!(
        summary_len = compact_result.summary.len(),
        messages_used = compact_result.messages_used,
        "compact_task: Full Compact 完成"
    );

    let re_inject_result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            tracing::info!("compact_task: re_inject 阶段被取消");
            if tx.send(super::AgentEvent::CompactError("已取消".to_string())).await.is_err() {
                warn!("compact_task: failed to send CompactError (channel closed)");
            }
            fire_standalone_lifecycle_hooks(
                &registered_hooks,
                HookEvent::PostCompact,
                &cwd,
                &session_id,
                &transcript_path,
                &provider_name,
                Some(msg_count),
            )
            .await;
            return;
        }
        result = re_inject(&messages, &config, &cwd) => result,
    };

    tracing::info!(
        files_injected = re_inject_result.files_injected,
        skills_injected = re_inject_result.skills_injected,
        "compact_task: 重新注入完成"
    );

    // compact_result.summary 已包含 postprocess_summary 添加的前缀，无需重复添加
    let summary_text = compact_result.summary;

    let re_inject_content = if re_inject_result.messages.is_empty() {
        String::new()
    } else {
        let mut parts = Vec::new();
        for msg in &re_inject_result.messages {
            parts.push(msg.content());
        }
        // 使用唯一分隔符避免文件内容中的空行被错误分割
        format!(
            "\n\n---RE_INJECT_SEPARATOR---\n{}",
            parts.join("\n---RE_INJECT_MSG_BREAK---\n")
        )
    };

    let combined_summary = format!("{}{}", summary_text, re_inject_content);

    // Fire PostCompact hooks on success
    fire_standalone_lifecycle_hooks(
        &registered_hooks,
        HookEvent::PostCompact,
        &cwd,
        &session_id,
        &transcript_path,
        &provider_name,
        Some(msg_count),
    )
    .await;

    if tx
        .send(super::AgentEvent::CompactCompleted {
            summary: combined_summary,
            files: vec![],
            skills: vec![],
            micro_cleared: 0,
            messages: vec![],
        })
        .await
        .is_err()
    {
        warn!("compact_task: failed to send CompactDone (channel closed)");
    }
}
