use std::path::PathBuf;
use std::sync::Arc;

use rust_agent_middlewares::mcp::McpClientPool;
use rust_agent_middlewares::mcp::McpInitStatus;
use rust_agent_middlewares::plugin::PluginLoadResult;
use rust_agent_middlewares::prelude::SharedPermissionMode;

use super::cron_state::CronState;
use super::events::AgentEvent;
use super::oauth_prompt::OAuthPrompt;
use super::setup_wizard::SetupWizardPanel;
use crate::config::PeriConfig;
use crate::thread::ThreadStore;

/// 全局服务/状态聚合：跨 session 共享的服务字段。
pub struct ServiceRegistry {
    pub peri_config: Option<PeriConfig>,
    pub cwd: String,
    pub provider_name: String,
    pub model_name: String,
    pub permission_mode: Arc<SharedPermissionMode>,
    pub thread_store: Arc<dyn ThreadStore>,
    pub mcp_pool: Option<Arc<McpClientPool>>,
    pub mcp_init_rx: Option<tokio::sync::watch::Receiver<McpInitStatus>>,
    pub cron: CronState,
    pub plugin_data: Option<PluginLoadResult>,
    pub bg_event_tx: tokio::sync::mpsc::Sender<AgentEvent>,
    pub bg_event_rx: Option<tokio::sync::mpsc::Receiver<AgentEvent>>,
    pub config_path_override: Option<PathBuf>,
    pub claude_settings_override: Option<PathBuf>,
    pub setup_wizard: Option<SetupWizardPanel>,
    pub oauth_prompt: Option<OAuthPrompt>,
    pub mode_highlight_until: Option<std::time::Instant>,
    pub model_highlight_until: Option<std::time::Instant>,
    pub mcp_ready_shown_until: std::cell::Cell<Option<std::time::Instant>>,
    pub quit_pending_since: Option<std::time::Instant>,
}
