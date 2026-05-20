//! Agent lifecycle handlers — cleanup, done, interrupted, error.
//! Extracted from original agent_ops.rs (2026-05-20 split).

use super::super::*;
use tracing::debug;

use crate::app::message_pipeline::PipelineAction;
use crate::app::App;

impl App {
    /// Shared agent state teardown for Done, Error, and Disconnected paths.
    /// Ends the Langfuse trace, sets loading=false, clears interaction state,
    /// and records task duration. Callers handle bg task channel logic separately.
    pub(super) fn cleanup_agent_state(&mut self, langfuse_error: Option<&str>) {
        {
            let s = &mut self.session_mgr.sessions[self.session_mgr.active];

            // End Langfuse trace
            let tracer = s.langfuse.langfuse_tracer.take();
            if let Some(ref t) = tracer {
                s.langfuse.langfuse_flush_handle = Some(t.lock().on_trace_end(langfuse_error));
            }
            s.langfuse.langfuse_tracer = None;

            // Clear interaction state
            s.agent.interaction_prompt = None;
            s.agent.pending_hitl_items = None;
            s.agent.pending_ask_user = None;

            // Record task duration
            if let Some(start) = s.agent.task_start_time {
                s.agent.last_task_duration = Some(start.elapsed());
            }
        }
        self.set_loading(false);
    }

    pub(super) fn handle_done(&mut self) -> (bool, bool, bool) {
        // Child agent Done during tool execution — ignore
        let in_sub = self.session_mgr.sessions[self.session_mgr.active]
            .messages
            .pipeline
            .in_subagent();
        debug!(
            in_subagent = in_sub,
            active = self.session_mgr.active,
            "AgentEvent::Done — checking in_subagent"
        );
        if in_sub {
            return (false, false, false);
        }
        self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .retry_status = None;
        // Pipeline：finalize 当前 AI 消息
        let actions = self.session_mgr.sessions[self.session_mgr.active]
            .messages
            .pipeline
            .handle_event(AgentEvent::Done);
        for action in actions {
            self.apply_pipeline_action(action);
        }
        // 跳过已由 Interrupted/Error 处理器完成的 reconcile
        if !self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .reconcile_already_done
        {
            self.request_rebuild();
        } else {
            if let Some(MessageViewModel::AssistantBubble { is_streaming, .. }) =
                self.session_mgr.sessions[self.session_mgr.active]
                    .messages
                    .view_messages
                    .last_mut()
            {
                *is_streaming = false;
            }
            self.render_rebuild();
        }
        // 后台任务：保持通道存活
        if self.session_mgr.sessions[self.session_mgr.active].background_task_count > 0 {
            self.session_mgr.sessions[self.session_mgr.active]
                .agent
                .agent_done_pending_bg = true;
            tracing::info!(
                count = self.session_mgr.sessions[self.session_mgr.active].background_task_count,
                "agent done but background tasks still running, keeping channel alive"
            );
        } else {
            // 竞态修复：处理暂存的后台任务完成通知
            if !self.session_mgr.sessions[self.session_mgr.active]
                .agent
                .pre_done_bg_completions
                .is_empty()
            {
                let notifications: Vec<String> = self.session_mgr.sessions[self.session_mgr.active]
                    .agent
                    .pre_done_bg_completions
                    .drain(..)
                    .collect();
                let combined = notifications.join("\n");
                tracing::info!(
                    count = notifications.len(),
                    "Done: processing pre-done background task completions, setting continuation"
                );
                self.session_mgr.sessions[self.session_mgr.active]
                    .agent
                    .pending_bg_continuation = Some(combined);
            }
            self.session_mgr.sessions[self.session_mgr.active]
                .agent
                .agent_rx = None;
        }
        self.cleanup_agent_state(None);
        // circuit breaker 渐进恢复：每轮成功对话将 failure 计数减半
        if self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .auto_compact_failures
            > 0
        {
            self.session_mgr.sessions[self.session_mgr.active]
                .agent
                .auto_compact_failures /= 2;
        }
        // 检查缓冲消息，合并发送
        if !self.session_mgr.sessions[self.session_mgr.active]
            .messages
            .pending_messages
            .is_empty()
        {
            self.flush_pending_messages();
        }
        (true, false, true)
    }

    pub(super) fn handle_interrupted(&mut self) -> (bool, bool, bool) {
        // Child agent interrupted during tool execution — ignore
        if self.session_mgr.sessions[self.session_mgr.active]
            .messages
            .pipeline
            .in_subagent()
        {
            return (false, false, false);
        }
        // Pipeline：finalize 当前状态
        let actions = self.session_mgr.sessions[self.session_mgr.active]
            .messages
            .pipeline
            .handle_event(AgentEvent::Interrupted);
        for action in actions {
            self.apply_pipeline_action(action);
        }

        let agent_replied = self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .agent_replied;
        if !agent_replied {
            // Agent 尚未回复，恢复用户文本到输入框
            if let Some(text) = self.session_mgr.sessions[self.session_mgr.active]
                .messages
                .last_submitted_text
                .take()
            {
                let round_start = self.session_mgr.sessions[self.session_mgr.active]
                    .messages
                    .round_start_vm_idx;
                // 截断 view_messages（移除本轮 Human 消息）
                self.apply_pipeline_action(PipelineAction::RebuildAll {
                    prefix_len: round_start,
                    tail_vms: vec![],
                });
                // 截断 agent_state_messages（回滚 StateSnapshot 扩展的内容）
                let pre_len = self.session_mgr.sessions[self.session_mgr.active]
                    .metadata
                    .pre_submit_state_len;
                self.session_mgr.sessions[self.session_mgr.active]
                    .agent
                    .agent_state_messages
                    .truncate(pre_len);
                // 恢复文本到输入框
                let mut ta = crate::app::build_textarea(false);
                ta.insert_str(text.clone());
                self.session_mgr.sessions[self.session_mgr.active]
                    .ui
                    .textarea = ta;
                // 清除 pending 缓冲
                self.session_mgr.sessions[self.session_mgr.active]
                    .messages
                    .pending_messages
                    .clear();
                // 清除 sticky header
                self.session_mgr.sessions[self.session_mgr.active]
                    .metadata
                    .last_human_message = None;
                // 清除 pipeline 状态（completed 中含本轮 Human 消息）
                self.session_mgr.sessions[self.session_mgr.active]
                    .messages
                    .pipeline
                    .done();
                let restored = self.session_mgr.sessions[self.session_mgr.active]
                    .agent
                    .agent_state_messages
                    .clone();
                self.session_mgr.sessions[self.session_mgr.active]
                    .messages
                    .pipeline
                    .restore_completed(restored);
                let vm = MessageViewModel::system(self.services.lc.tr("app-interrupted-resumed"));
                self.apply_pipeline_action(PipelineAction::AddMessage(vm));
            } else {
                let vm = MessageViewModel::system(self.services.lc.tr("app-interrupt-done"));
                self.apply_pipeline_action(PipelineAction::AddMessage(vm));
            }
        } else {
            self.request_rebuild();
            let vm = MessageViewModel::system(self.services.lc.tr("app-interrupt-done"));
            self.apply_pipeline_action(PipelineAction::AddMessage(vm));
            // 标记 reconcile 已完成，防止后续 Done 事件重复 RebuildAll 覆盖通知消息
            self.session_mgr.sessions[self.session_mgr.active]
                .agent
                .reconcile_already_done = true;
        }
        (true, false, false)
    }

    pub(super) fn handle_error(&mut self, error_msg: &str) -> (bool, bool, bool) {
        // Child agent error during tool execution — ignore
        if self.session_mgr.sessions[self.session_mgr.active]
            .messages
            .pipeline
            .in_subagent()
        {
            return (false, false, false);
        }
        self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .retry_status = None;
        // 清理 pipeline 状态（残留 SubAgent 栈等），防止下一个任务 UI 损坏
        self.session_mgr.sessions[self.session_mgr.active]
            .messages
            .pipeline
            .done();

        let mut vm = MessageViewModel::tool_block(
            "error".to_string(),
            "Agent Error".to_string(),
            None,
            true,
        );
        if let MessageViewModel::ToolBlock {
            content, collapsed, ..
        } = &mut vm
        {
            *content = error_msg.to_string();
            *collapsed = false;
        }
        self.apply_pipeline_action(PipelineAction::AddMessage(vm));
        // 标记 reconcile 已完成，防止后续 Done 事件重复 RebuildAll 覆盖错误消息
        self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .reconcile_already_done = true;
        // 后台任务：保持通道存活
        if self.session_mgr.sessions[self.session_mgr.active].background_task_count > 0 {
            self.session_mgr.sessions[self.session_mgr.active]
                .agent
                .agent_done_pending_bg = true;
        } else {
            if !self.session_mgr.sessions[self.session_mgr.active]
                .agent
                .pre_done_bg_completions
                .is_empty()
            {
                let notifications: Vec<String> = self.session_mgr.sessions[self.session_mgr.active]
                    .agent
                    .pre_done_bg_completions
                    .drain(..)
                    .collect();
                let combined = notifications.join("\n");
                tracing::info!(
                    count = notifications.len(),
                    "Error: processing pre-done background task completions, setting continuation"
                );
                self.session_mgr.sessions[self.session_mgr.active]
                    .agent
                    .pending_bg_continuation = Some(combined);
            }
            self.session_mgr.sessions[self.session_mgr.active]
                .agent
                .agent_rx = None;
        }
        let err_label = format!("ERROR: {}", error_msg);
        self.cleanup_agent_state(Some(&err_label));
        // 检查缓冲消息，合并发送
        if !self.session_mgr.sessions[self.session_mgr.active]
            .messages
            .pending_messages
            .is_empty()
        {
            self.flush_pending_messages();
        }
        (true, false, true)
    }

    // handle_agent_event is in mod.rs
}
