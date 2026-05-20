//! ACP notification bridge — converts AcpNotification → TUI AgentEvent dispatch.
//! Extracted from original agent_ops.rs (2026-05-20 split).

use super::super::*;
use tracing::debug;

use crate::app::App;

impl App {
    /// 处理 ACP notification — 将 AcpNotification 转换为相应的 UI 操作。
    /// 返回 `(updated, should_break, should_return)`，与 `handle_agent_event` 相同语义。
    pub(crate) fn handle_acp_notification(&mut self, notif: AcpNotification) -> (bool, bool, bool) {
        match notif {
            AcpNotification::AgentEvent { event, session_id } => {
                // Convert peri-agent ExecutorEvent → TUI AgentEvent via map_executor_event
                if let Some(agent_event) =
                    super::super::agent::map_executor_event(event, &self.services.cwd)
                {
                    debug!(
                        session_id = %session_id,
                        "ACP→TUI: AgentEvent dispatched to handle_agent_event"
                    );
                    return self.handle_agent_event(agent_event);
                }
                debug!(
                    session_id = %session_id,
                    "ACP→TUI: ExecutorEvent filtered by map_executor_event (internal event)"
                );
                (false, false, false)
            }
            AcpNotification::AgentDone { session_id } => {
                debug!(session_id = %session_id, "ACP→TUI: AgentDone received");
                self.handle_agent_event(super::super::AgentEvent::Done)
            }
            AcpNotification::RequestPermission { id, params } => {
                self.handle_acp_request_permission(id, params)
            }
            AcpNotification::Elicitation { id, params } => self.handle_acp_elicitation(id, params),
            AcpNotification::SessionUpdate { .. } => (false, false, false),
            AcpNotification::Peri { method, params, .. } => {
                tracing::debug!(%method, "ACP→TUI: peri/* notification (no TUI action)");
                let _ = params;
                (false, false, false)
            }
            AcpNotification::Other { msg } => {
                tracing::warn!(%msg, "Unhandled ACP notification");
                (false, false, false)
            }
        }
    }
}
