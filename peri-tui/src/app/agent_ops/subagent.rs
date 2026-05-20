//! SubAgent state tracking — token usage updates + subagent start events.
//! Extracted from original agent_ops.rs (2026-05-20 split).

use super::super::*;

use crate::app::message_pipeline::PipelineAction;
use crate::app::App;

impl App {
    pub(super) fn handle_token_usage_update(
        &mut self,
        usage: peri_agent::llm::types::TokenUsage,
    ) -> (bool, bool, bool) {
        // SubAgent 的 TokenUsageUpdate 不应污染父 agent 的 tracker
        if self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .subagent_depth
            > 0
        {
            return (true, false, false);
        }

        // 累积到会话追踪器
        self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .session_token_tracker
            .accumulate(&usage);

        // 缓存率检查：当次命中率低于 80% 时显示黄色提示
        let rate = self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .session_token_tracker
            .cache_hit_rate();
        if rate < 0.8 {
            let tracker = &self.session_mgr.sessions[self.session_mgr.active]
                .agent
                .session_token_tracker;
            tracing::warn!(
                input = tracker.total_input_tokens,
                cache_read = tracker.total_cache_read_tokens,
                rate_pct = rate * 100.0,
                "prompt cache hit rate below threshold"
            );
            let percentage = (rate * 100.0) as u32;
            let req_id = tracker.last_request_id.as_deref().unwrap_or("-");
            let msg = format!(
                "⚠ {}",
                self.services.lc.tr_args(
                    "app-prompt-cache-low",
                    &[
                        ("rate".into(), (percentage as i64).into()),
                        ("req".into(), req_id.to_string().into()),
                    ]
                )
            );
            let vm = MessageViewModel::system(msg);
            self.apply_pipeline_action(PipelineAction::AddMessage(vm));
        }
        // 更新 spinner 的 token 显示（仅当次调用的 token，不累计）
        let current_tokens = usage.input_tokens as usize + usage.output_tokens as usize;
        self.session_mgr.sessions[self.session_mgr.active]
            .spinner_state
            .set_token_count(current_tokens);
        (true, false, false)
    }

    pub(super) fn handle_subagent_start(
        &mut self,
        agent_id: String,
        instance_id: String,
        task_preview: String,
        is_background: bool,
    ) -> (bool, bool, bool) {
        if is_background {
            self.session_mgr.sessions[self.session_mgr.active].background_task_count += 1;
        }
        self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .subagent_depth += 1;
        // 跨切面：Langfuse
        if let Some(ref tracer) = self.session_mgr.sessions[self.session_mgr.active]
            .langfuse
            .langfuse_tracer
        {
            let _lock_start = std::time::Instant::now();
            tracer.lock().on_subagent_start(&agent_id, &task_preview);
            let _wait = _lock_start.elapsed();
            if _wait.as_millis() > 50 {
                tracing::warn!(
                    "[DEADLOCK] TUI: tracer lock held {:?} for SubAgentStart({})",
                    _wait,
                    agent_id
                );
            }
        }
        // Pipeline：创建 SubAgentGroup VM
        let actions = self.session_mgr.sessions[self.session_mgr.active]
            .messages
            .pipeline
            .handle_event(AgentEvent::SubAgentStart {
                agent_id,
                instance_id,
                task_preview,
                is_background,
            });
        for action in actions {
            self.apply_pipeline_action(action);
        }
        self.request_rebuild();
        (true, false, false)
    }
}
