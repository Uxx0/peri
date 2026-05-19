use super::message_pipeline::PipelineAction;
use super::*;
use peri_agent::agent::events::CompactFileInfo;
use peri_agent::messages::BaseMessage;

impl App {
    pub(crate) fn handle_compact_started(&mut self) -> (bool, bool, bool) {
        let vm = MessageViewModel::system(self.services.lc.tr("app-compact-started"));
        self.apply_pipeline_action(PipelineAction::AddMessage(vm));
        (true, false, false)
    }

    pub(crate) fn handle_compact_completed(
        &mut self,
        _summary: String,
        files: Vec<CompactFileInfo>,
        skills: Vec<String>,
        micro_cleared: usize,
        messages: Vec<BaseMessage>,
    ) -> (bool, bool, bool) {
        if micro_cleared > 0 {
            // Micro-compact: 更新内部状态，保留 pipeline 显示
            self.session_mgr.sessions[self.session_mgr.active]
                .agent
                .agent_state_messages = messages;
            let vm = MessageViewModel::system(self.services.lc.tr_args(
                "app-compact-auto-cleared",
                &[("count".into(), (micro_cleared as i64).into())],
            ));
            self.apply_pipeline_action(PipelineAction::AddMessage(vm));
            return (true, false, false);
        }

        // Full compact: 用压缩后的消息替换 pipeline 和内部状态
        let mut label_lines = vec![format!("✻ {}", self.services.lc.tr("app-compact-done"))];
        for f in &files {
            label_lines.push(format!("  ⎿  Read {} ({} lines)", f.path, f.lines));
        }
        if !skills.is_empty() {
            label_lines.push(format!("  ⎿  Skill: {}", skills.join(", ")));
        }
        let compact_label = label_lines.join("\n");

        // 更新内部状态消息
        self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .agent_state_messages = messages.clone();

        // 清除 ephemeral_notes，防止 compact 前的系统通知残留
        self.session_mgr.sessions[self.session_mgr.active]
            .messages
            .ephemeral_notes
            .clear();

        // 清空 pipeline 并用压缩后的消息恢复
        self.session_mgr.sessions[self.session_mgr.active]
            .messages
            .pipeline
            .clear();
        self.session_mgr.sessions[self.session_mgr.active]
            .messages
            .pipeline
            .restore_completed(messages);

        // 显示 compact 通知
        let view_msgs = vec![MessageViewModel::system(compact_label)];
        self.apply_pipeline_action(PipelineAction::RebuildAll {
            prefix_len: 0,
            tail_vms: view_msgs,
        });

        self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .pre_compact_token_snapshot = None;
        self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .auto_compact_failures = 0;

        (true, false, false)
    }

    pub(crate) fn handle_compact_error(&mut self, msg: String) -> (bool, bool, bool) {
        let vm = MessageViewModel::system(
            self.services
                .lc
                .tr_args("app-compact-failed", &[("error".into(), msg.into())]),
        );
        self.apply_pipeline_action(PipelineAction::AddMessage(vm));

        if let Some(snapshot) = self.session_mgr.sessions[self.session_mgr.active]
            .agent
            .pre_compact_token_snapshot
            .take()
        {
            self.session_mgr.sessions[self.session_mgr.active]
                .agent
                .session_token_tracker = snapshot;
        }

        (true, false, false)
    }
}
