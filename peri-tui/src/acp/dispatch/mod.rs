pub mod auth;
pub mod fallback;
pub mod helpers;
pub mod prompt;
pub mod session;

pub use auth::{handle_authenticate, handle_logout};
pub use fallback::handle_dispatch;
pub use prompt::handle_prompt;
pub use session::{
    handle_close_session, handle_fork_session, handle_list_sessions, handle_load_session,
    handle_new_session, handle_resume_session, handle_set_config_option, handle_set_mode,
    handle_set_model,
};

use tokio::sync::OnceCell;

use super::session::SessionManager;

static SESSION_MANAGER: OnceCell<SessionManager> = OnceCell::const_new();

/// 初始化全局 SessionManager（必须在 Agent::builder().connect_to() 之前调用）
pub fn init_session_manager(mgr: SessionManager) {
    let _ = SESSION_MANAGER.set(mgr);
}

pub(crate) fn mgr() -> &'static SessionManager {
    SESSION_MANAGER
        .get()
        .expect("SessionManager not initialized")
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::{
        ModelId, ModelInfo, NewSessionResponse, SessionConfigId, SessionConfigKind,
        SessionConfigSelect, SessionConfigSelectOption, SessionConfigValueId, SessionMode,
        SessionModeId, SessionModeState, SessionModelState,
    };
    include!("dispatch_test.rs");
}
