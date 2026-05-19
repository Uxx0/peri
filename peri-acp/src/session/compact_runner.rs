//! Compact execution logic shared by auto-compact and manual session/compact.
//!
//! Wraps `peri_agent::agent::compact::{full_compact, micro_compact_enhanced, re_inject}`
//! with hook firing, event sending, and cancellation support.

use std::sync::Arc;

use peri_agent::agent::compact::config::CompactConfig;
use peri_agent::agent::compact::{full_compact, micro_compact_enhanced, re_inject, ReInjectResult};
use peri_agent::agent::events::{AgentEvent as ExecutorEvent, CompactFileInfo};
use peri_agent::agent::AgentCancellationToken;
use peri_agent::llm::BaseModel;
use peri_agent::messages::BaseMessage;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Compact 执行结果
pub struct CompactOutput {
    /// 压缩后的新消息列表（summary + re_inject messages）
    pub new_messages: Vec<BaseMessage>,
    /// 摘要文本
    pub summary: String,
    /// 保留的文件信息
    pub files: Vec<CompactFileInfo>,
    /// 保留的 Skill 名称
    pub skills: Vec<String>,
}

/// Hook 上下文信息
pub struct HookContext {
    pub cwd: String,
    pub session_id: String,
    pub transcript_path: String,
    pub provider_name: String,
    /// 可选的 compact 指令（手动 /compact 传入）
    pub instructions: String,
}

/// 通过 event_tx 发送事件
fn send_event(
    event_tx: &Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<ExecutorEvent>>>>,
    event: ExecutorEvent,
) {
    if let Some(tx) = event_tx.lock().unwrap().as_ref() {
        let _ = tx.send(event);
    }
}

/// 执行 full compact：full_compact + re_inject + hooks + 事件通知
#[allow(clippy::too_many_arguments)]
pub async fn run_full_compact(
    messages: &[BaseMessage],
    model: &dyn BaseModel,
    config: &CompactConfig,
    cwd: &str,
    event_tx: &Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<ExecutorEvent>>>>,
    cancel: &AgentCancellationToken,
    hooks: &[peri_middlewares::hooks::types::RegisteredHook],
    hook_ctx: &HookContext,
) -> Result<CompactOutput, String> {
    let msg_count = messages.len();
    info!(msg_count, "compact_runner: starting full compact");

    // Fire PreCompact hooks
    fire_post_compact_hooks_inner(
        hooks,
        peri_middlewares::hooks::types::HookEvent::PreCompact,
        hook_ctx,
        msg_count,
    )
    .await;

    send_event(event_tx, ExecutorEvent::CompactStarted);

    // full_compact with cancellation
    let compact_result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            send_event(event_tx, ExecutorEvent::CompactError {
                message: "已取消".to_string(),
            });
            fire_post_compact_hooks(hooks, hook_ctx, msg_count).await;
            return Err("已取消".to_string());
        }
        result = full_compact(messages, model, config, &hook_ctx.instructions) => {
            match result {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "compact_runner: full_compact failed");
                    send_event(event_tx, ExecutorEvent::CompactError {
                        message: e.to_string(),
                    });
                    fire_post_compact_hooks(hooks, hook_ctx, msg_count).await;
                    return Err(e.to_string());
                }
            }
        }
    };

    // Cancel check before re_inject
    if cancel.is_cancelled() {
        send_event(
            event_tx,
            ExecutorEvent::CompactError {
                message: "已取消".to_string(),
            },
        );
        fire_post_compact_hooks(hooks, hook_ctx, msg_count).await;
        return Err("已取消".to_string());
    }

    info!(
        summary_len = compact_result.summary.len(),
        messages_used = compact_result.messages_used,
        "compact_runner: full_compact completed"
    );

    // re_inject
    let re_inject_result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            send_event(event_tx, ExecutorEvent::CompactError {
                message: "已取消".to_string(),
            });
            fire_post_compact_hooks(hooks, hook_ctx, msg_count).await;
            return Err("已取消".to_string());
        }
        result = re_inject(messages, config, cwd) => result,
    };

    info!(
        files_injected = re_inject_result.files_injected,
        skills_injected = re_inject_result.skills_injected,
        "compact_runner: re_inject completed"
    );

    // Extract file and skill info before moving messages
    let files = extract_file_info(&re_inject_result);
    let skills = extract_skill_names(&re_inject_result);

    // Build new messages
    let mut new_messages = vec![BaseMessage::system(compact_result.summary.clone())];
    new_messages.extend(re_inject_result.messages);

    send_event(
        event_tx,
        ExecutorEvent::CompactCompleted {
            summary: compact_result.summary.clone(),
            files: files.clone(),
            skills: skills.clone(),
            micro_cleared: 0,
            messages: new_messages.clone(),
        },
    );

    fire_post_compact_hooks(hooks, hook_ctx, msg_count).await;

    Ok(CompactOutput {
        new_messages,
        summary: compact_result.summary,
        files,
        skills,
    })
}

/// 执行 micro-compact：原地修改 messages，发送 CompactCompleted 事件
pub fn run_micro_compact(
    messages: &mut [BaseMessage],
    config: &CompactConfig,
    event_tx: &Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<ExecutorEvent>>>>,
) -> usize {
    let cleared = micro_compact_enhanced(config, messages);
    if cleared > 0 {
        info!(cleared, "compact_runner: micro-compact completed");
        send_event(
            event_tx,
            ExecutorEvent::CompactCompleted {
                summary: String::new(),
                files: vec![],
                skills: vec![],
                micro_cleared: cleared,
                messages: messages.to_vec(),
            },
        );
    }
    cleared
}

async fn fire_post_compact_hooks(
    hooks: &[peri_middlewares::hooks::types::RegisteredHook],
    ctx: &HookContext,
    msg_count: usize,
) {
    fire_post_compact_hooks_inner(
        hooks,
        peri_middlewares::hooks::types::HookEvent::PostCompact,
        ctx,
        msg_count,
    )
    .await
}

async fn fire_post_compact_hooks_inner(
    hooks: &[peri_middlewares::hooks::types::RegisteredHook],
    event: peri_middlewares::hooks::types::HookEvent,
    ctx: &HookContext,
    msg_count: usize,
) {
    peri_middlewares::hooks::middleware::fire_standalone_lifecycle_hooks(
        hooks,
        event,
        &ctx.cwd,
        &ctx.session_id,
        &ctx.transcript_path,
        &ctx.provider_name,
        Some(msg_count),
    )
    .await
}

/// 从 re_inject 结果中提取文件信息
fn extract_file_info(re_inject_result: &ReInjectResult) -> Vec<CompactFileInfo> {
    let mut files = Vec::new();
    for msg in &re_inject_result.messages {
        let content = msg.content();
        if let Some(rest) = content.strip_prefix("[最近读取的文件: ") {
            let path = rest.lines().next().unwrap_or("");
            let line_count = rest.lines().count().saturating_sub(1);
            if !path.is_empty() {
                files.push(CompactFileInfo {
                    path: path.to_string(),
                    lines: line_count,
                });
            }
        }
    }
    files
}

/// 从 re_inject 结果中提取 skill 名称
fn extract_skill_names(re_inject_result: &ReInjectResult) -> Vec<String> {
    let mut skills = Vec::new();
    for msg in &re_inject_result.messages {
        let content = msg.content();
        if let Some(rest) = content.strip_prefix("[激活的 Skill 指令: ") {
            let name = rest.lines().next().unwrap_or("");
            if !name.is_empty() {
                skills.push(name.to_string());
            }
        }
    }
    skills
}
