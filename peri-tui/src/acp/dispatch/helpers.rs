use agent_client_protocol::schema::{
    ContentBlock, LoadSessionResponse, ModelId, ModelInfo, NewSessionResponse, SessionConfigId,
    SessionConfigKind, SessionConfigOptionCategory, SessionConfigSelect, SessionConfigSelectOption,
    SessionConfigValueId, SessionId, SessionMode, SessionModeId, SessionModeState,
    SessionModelState, SessionNotification, SessionUpdate, TextContent, ToolCall, ToolCallContent,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::Client;
use peri_agent::messages::BaseMessage;
use peri_middlewares::prelude::PermissionMode;

use crate::config::PeriConfig;

use super::super::session::AcpSession;
use super::mgr;

// ─── Helper: 构建 session 元数据 ─────────────────────────────────────────────

pub(crate) fn build_session_mode_state(session: &AcpSession) -> SessionModeState {
    let current = match session.permission_mode.load() {
        PermissionMode::Default => "default",
        PermissionMode::DontAsk => "dontAsk",
        PermissionMode::AcceptEdit => "acceptEdits",
        PermissionMode::AutoMode => "auto",
        PermissionMode::Bypass => "bypass",
    };
    let mut state = SessionModeState::new(
        SessionModeId::new(current),
        vec![
            SessionMode::new(SessionModeId::new("auto"), "Auto")
                .description("LLM classifier decides approval"),
            SessionMode::new(SessionModeId::new("default"), "Default")
                .description("Approval for sensitive tools"),
            SessionMode::new(SessionModeId::new("acceptEdits"), "Accept Edits")
                .description("Allow file edits without approval"),
            SessionMode::new(SessionModeId::new("dontAsk"), "Don't Ask")
                .description("Agent answers only, no tool execution"),
            SessionMode::new(SessionModeId::new("bypass"), "Bypass")
                .description("Full tool access, no approval needed"),
        ],
    );
    let _ = &mut state; // silence non-exhaustive warnings
    state
}

pub(crate) fn build_session_model_state(
    session: &AcpSession,
    peri_config: &PeriConfig,
) -> SessionModelState {
    let provider = peri_config
        .config
        .providers
        .iter()
        .find(|p| p.id == peri_config.config.active_provider_id);
    let current = session.model_alias.clone();
    let mut models = vec![];
    for alias in &["opus", "sonnet", "haiku"] {
        if let Some(name) =
            provider.and_then(|p| p.models.get_model(alias).filter(|m| !m.is_empty()))
        {
            models.push(ModelInfo::new(ModelId::new(*alias), name));
        }
    }
    SessionModelState::new(ModelId::new(current), models)
}

pub(crate) fn build_config_options(
    session: &AcpSession,
) -> Vec<agent_client_protocol::schema::SessionConfigOption> {
    let peri_config = mgr().peri_config();

    // 1. Mode selector
    let current_mode = match session.permission_mode.load() {
        PermissionMode::Default => "default",
        PermissionMode::DontAsk => "dontAsk",
        PermissionMode::AcceptEdit => "acceptEdits",
        PermissionMode::AutoMode => "auto",
        PermissionMode::Bypass => "bypass",
    };
    let mode_option = agent_client_protocol::schema::SessionConfigOption::select(
        SessionConfigId::new("mode"),
        "Mode",
        SessionConfigValueId::new(current_mode),
        vec![
            SessionConfigSelectOption::new(SessionConfigValueId::new("auto"), "Auto"),
            SessionConfigSelectOption::new(SessionConfigValueId::new("default"), "Default"),
            SessionConfigSelectOption::new(
                SessionConfigValueId::new("acceptEdits"),
                "Accept Edits",
            ),
            SessionConfigSelectOption::new(SessionConfigValueId::new("dontAsk"), "Don't Ask"),
            SessionConfigSelectOption::new(SessionConfigValueId::new("bypass"), "Bypass"),
        ],
    )
    .category(SessionConfigOptionCategory::Mode)
    .description("Permission mode for tool execution");

    // 2. Model selector
    let provider = peri_config
        .config
        .providers
        .iter()
        .find(|p| p.id == peri_config.config.active_provider_id);
    let mut model_options = vec![];
    for alias in &["opus", "sonnet", "haiku"] {
        if let Some(name) =
            provider.and_then(|p| p.models.get_model(alias).filter(|m| !m.is_empty()))
        {
            model_options.push(SessionConfigSelectOption::new(
                SessionConfigValueId::new(*alias),
                name,
            ));
        }
    }
    let model_option = agent_client_protocol::schema::SessionConfigOption::select(
        SessionConfigId::new("model"),
        "Model",
        SessionConfigValueId::new(session.model_alias.as_str()),
        model_options,
    )
    .category(SessionConfigOptionCategory::Model)
    .description("AI model for this session");

    // 3. Thinking effort selector
    let effort_val = session
        .thinking
        .as_ref()
        .map(|t| t.effort.as_str())
        .unwrap_or("high");
    let thinking_option = agent_client_protocol::schema::SessionConfigOption::new(
        SessionConfigId::new("thinking_effort"),
        "Thinking Effort",
        SessionConfigKind::Select(SessionConfigSelect::new(
            SessionConfigValueId::new(effort_val),
            vec![
                SessionConfigSelectOption::new(SessionConfigValueId::new("low"), "Low"),
                SessionConfigSelectOption::new(SessionConfigValueId::new("medium"), "Medium"),
                SessionConfigSelectOption::new(SessionConfigValueId::new("high"), "High"),
                SessionConfigSelectOption::new(SessionConfigValueId::new("xhigh"), "XHigh"),
                SessionConfigSelectOption::new(SessionConfigValueId::new("max"), "Max"),
            ],
        )),
    )
    .category(SessionConfigOptionCategory::ThoughtLevel)
    .description("Controls reasoning depth");

    vec![mode_option, model_option, thinking_option]
}

/// 填充 NewSessionResponse 元数据
pub(crate) fn fill_new_session_resp(
    session: &AcpSession,
    mut resp: NewSessionResponse,
) -> NewSessionResponse {
    resp = resp.modes(Some(build_session_mode_state(session)));
    resp = resp.config_options(Some(build_config_options(session)));
    resp = resp.models(Some(build_session_model_state(
        session,
        mgr().peri_config(),
    )));
    resp
}

/// 填充 LoadSessionResponse 元数据
pub(crate) fn fill_load_session_resp(
    session: &AcpSession,
    mut resp: LoadSessionResponse,
) -> LoadSessionResponse {
    resp = resp.modes(Some(build_session_mode_state(session)));
    resp = resp.config_options(Some(build_config_options(session)));
    resp = resp.models(Some(build_session_model_state(
        session,
        mgr().peri_config(),
    )));
    resp
}

// ─── Available Commands ────────────────────────────────────────────────────────

/// Build the list of available slash commands for AvailableCommandsUpdate.
pub(crate) fn build_available_commands() -> Vec<agent_client_protocol::schema::AvailableCommand> {
    vec![
        agent_client_protocol::schema::AvailableCommand::new(
            "help",
            "Show available commands and their descriptions",
        ),
        agent_client_protocol::schema::AvailableCommand::new(
            "clear",
            "Clear the conversation history",
        ),
        agent_client_protocol::schema::AvailableCommand::new(
            "compact",
            "Compress conversation context to save tokens",
        ),
        agent_client_protocol::schema::AvailableCommand::new(
            "cost",
            "Show token usage and cost summary",
        ),
        agent_client_protocol::schema::AvailableCommand::new(
            "context",
            "Show context window usage statistics",
        ),
        agent_client_protocol::schema::AvailableCommand::new("model", "Switch the active AI model"),
        agent_client_protocol::schema::AvailableCommand::new(
            "doctor",
            "Diagnose configuration and connectivity issues",
        ),
        agent_client_protocol::schema::AvailableCommand::new(
            "mcp",
            "Manage MCP (Model Context Protocol) servers",
        ),
        agent_client_protocol::schema::AvailableCommand::new("plugin", "Manage plugins"),
        agent_client_protocol::schema::AvailableCommand::new("hooks", "Manage hooks"),
        agent_client_protocol::schema::AvailableCommand::new("exit", "Exit the application"),
        agent_client_protocol::schema::AvailableCommand::new(
            "agents",
            "List available agent configurations",
        ),
        agent_client_protocol::schema::AvailableCommand::new(
            "history",
            "View conversation history",
        ),
        agent_client_protocol::schema::AvailableCommand::new(
            "loop",
            "Toggle auto-continue loop mode",
        ),
        agent_client_protocol::schema::AvailableCommand::new("cron", "Manage scheduled tasks"),
        agent_client_protocol::schema::AvailableCommand::new("memory", "View or edit agent memory"),
        agent_client_protocol::schema::AvailableCommand::new(
            "split",
            "Split current session into a new one",
        ),
        agent_client_protocol::schema::AvailableCommand::new(
            "effort",
            "Set thinking/reasoning effort level",
        ),
        agent_client_protocol::schema::AvailableCommand::new(
            "rename",
            "Rename the current session",
        ),
        agent_client_protocol::schema::AvailableCommand::new(
            "login",
            "Configure API provider authentication",
        ),
        agent_client_protocol::schema::AvailableCommand::new("lang", "Switch display language"),
        agent_client_protocol::schema::AvailableCommand::new("setup", "Run initial setup wizard"),
        agent_client_protocol::schema::AvailableCommand::new(
            "config",
            "View or edit configuration",
        ),
    ]
}

/// Send AvailableCommandsUpdate notification for a session.
pub(crate) fn send_available_commands(
    conn: &agent_client_protocol::ConnectionTo<Client>,
    session_id: &SessionId,
) {
    let update =
        agent_client_protocol::schema::AvailableCommandsUpdate::new(build_available_commands());
    let _ = conn.send_notification(SessionNotification::new(
        session_id.clone(),
        SessionUpdate::AvailableCommandsUpdate(update),
    ));
}

/// 将持久化的 BaseMessage 映射为 SessionUpdate（用于 session/load 回放）
pub(crate) fn map_message_to_updates(msg: &BaseMessage) -> Vec<SessionUpdate> {
    use agent_client_protocol::schema::ContentChunk;

    match msg {
        BaseMessage::Human { content, .. } => {
            let text = content.text_content();
            vec![SessionUpdate::UserMessageChunk(ContentChunk::new(
                ContentBlock::Text(TextContent::new(text)),
            ))]
        }
        BaseMessage::Ai {
            content,
            tool_calls,
            ..
        } => {
            let mut updates = Vec::new();

            // AI 文本消息
            let text = content.text_content();
            if !text.is_empty() {
                updates.push(SessionUpdate::AgentMessageChunk(ContentChunk::new(
                    ContentBlock::Text(TextContent::new(text)),
                )));
            }

            // 工具调用
            for tc in tool_calls {
                use agent_client_protocol::schema::Content;
                updates.push(SessionUpdate::ToolCall(
                    ToolCall::new(tc.id.clone(), tc.name.clone())
                        .status(ToolCallStatus::Completed)
                        .content(vec![ToolCallContent::Content(Content::new(
                            ContentBlock::Text(TextContent::new(truncate_str(
                                &tc.arguments.to_string(),
                                500,
                            ))),
                        ))]),
                ));
            }

            updates
        }
        BaseMessage::Tool {
            content,
            tool_call_id,
            is_error,
            ..
        } => {
            use agent_client_protocol::schema::Content;
            vec![SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                tool_call_id.clone(),
                ToolCallUpdateFields::new()
                    .status(if *is_error {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    })
                    .content(vec![ToolCallContent::Content(Content::new(
                        ContentBlock::Text(TextContent::new(truncate_str(
                            &content.text_content(),
                            500,
                        ))),
                    ))]),
            ))]
        }
        _ => vec![],
    }
}

pub(crate) fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let boundary = s.floor_char_boundary(max_len);
        format!("{}...", &s[..boundary])
    }
}
