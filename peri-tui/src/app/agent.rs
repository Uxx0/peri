// ── Live code retained after Task 6 ACP separation ──
// - map_executor_event: used by handle_acp_notification bridge (agent_ops.rs)

pub use super::provider::LlmProvider;
use super::AgentEvent;
use peri_agent::agent::events::AgentEvent as ExecutorEvent;

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
        // Agent ToolStart → 走普通 ToolStart 分支（注册 pending_tool）
        // SubAgentState 由后续的 SubagentStarted 事件创建（携带唯一 instance_id）
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
            child_thread_id: result.child_thread_id,
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
        // SubAgent 生命周期事件：SubagentStarted 创建 SubAgentState（携带唯一 instance_id）
        ExecutorEvent::SubagentStarted {
            agent_name,
            instance_id,
            is_background,
        } => AgentEvent::SubAgentStart {
            agent_id: agent_name.clone(),
            instance_id,
            task_preview: String::new(),
            is_background,
        },
        ExecutorEvent::SubagentStopped {
            agent_name,
            result,
            is_error,
            instance_id,
        } => AgentEvent::SubAgentEnd {
            agent_id: Some(agent_name),
            instance_id: Some(instance_id),
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
        ExecutorEvent::AgentExecutionFailed { message } => {
            if message == "Interrupted by user" {
                AgentEvent::Interrupted
            } else {
                AgentEvent::Error(message)
            }
        }
        ExecutorEvent::TodoUpdate(entries) => AgentEvent::TodoUpdate(
            entries
                .iter()
                .map(|e| {
                    use peri_middlewares::tools::todo::{TodoItem, TodoStatus as TuiTodoStatus};
                    TodoItem {
                        content: e.content.clone(),
                        active_form: e.active_form.clone(),
                        status: match e.status {
                            peri_agent::agent::events::TodoStatus::Pending => {
                                TuiTodoStatus::Pending
                            }
                            peri_agent::agent::events::TodoStatus::InProgress => {
                                TuiTodoStatus::InProgress
                            }
                            peri_agent::agent::events::TodoStatus::Completed => {
                                TuiTodoStatus::Completed
                            }
                        },
                    }
                })
                .collect(),
        ),
    })
}

#[cfg(test)]
#[path = "agent_test.rs"]
mod tests;
