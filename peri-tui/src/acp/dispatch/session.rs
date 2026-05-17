use agent_client_protocol::schema::{
    CloseSessionRequest, CloseSessionResponse, ConfigOptionUpdate, CurrentModeUpdate,
    ForkSessionRequest, ForkSessionResponse, ListSessionsRequest, ListSessionsResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, SessionId,
    SessionInfo, SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, SetSessionModeRequest, SetSessionModeResponse,
    SetSessionModelRequest, SetSessionModelResponse,
};
use agent_client_protocol::{Client, ConnectionTo};
use peri_middlewares::prelude::PermissionMode;

use super::helpers::{
    build_config_options, build_session_mode_state, build_session_model_state,
    fill_load_session_resp, fill_new_session_resp, map_message_to_updates, send_available_commands,
};
use super::mgr;

// ─── session/new handler ─────────────────────────────────────────────────────

pub async fn handle_new_session(
    req: NewSessionRequest,
    responder: agent_client_protocol::Responder<NewSessionResponse>,
    conn: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let cwd = req.cwd.to_string_lossy().to_string();

    match mgr().new_session(&cwd).await {
        Ok((session_id, _thread_id)) => {
            let resp = mgr()
                .get_session(&session_id)
                .map(|s| fill_new_session_resp(&s, NewSessionResponse::new(session_id.clone())))
                .unwrap_or_else(|| NewSessionResponse::new(session_id.clone()));

            let _ = responder.respond(resp);

            // Send available commands after session creation
            let sid = SessionId::new(session_id.clone());
            send_available_commands(&conn, &sid);
        }
        Err(e) => {
            tracing::error!("Failed to create session: {e}");
            let _ = responder.respond(NewSessionResponse::new(""));
        }
    }
    Ok(())
}

// ─── session/close handler ────────────────────────────────────────────────────

pub async fn handle_close_session(
    req: CloseSessionRequest,
    responder: agent_client_protocol::Responder<CloseSessionResponse>,
    _conn: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let session_id = req.session_id.0.as_ref();
    let _ = mgr().close_session(session_id).await;
    let _ = responder.respond(CloseSessionResponse::default());
    tracing::info!(session_id = %session_id, "ACP session closed");
    Ok(())
}

// ─── session/list handler ────────────────────────────────────────────────────

pub async fn handle_list_sessions(
    req: ListSessionsRequest,
    responder: agent_client_protocol::Responder<ListSessionsResponse>,
    _conn: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let cwd_filter = req.cwd.as_ref().map(|p| p.to_string_lossy().to_string());

    match mgr().list_sessions().await {
        Ok(threads) => {
            let sessions: Vec<SessionInfo> = threads
                .into_iter()
                .filter(|t| cwd_filter.as_ref().is_none_or(|cwd| t.cwd == *cwd))
                .map(|t| {
                    SessionInfo::new(SessionId::from(t.id), &t.cwd)
                        .title(t.title.unwrap_or_default())
                        .updated_at(t.updated_at.to_rfc3339())
                })
                .collect();
            let _ = responder.respond(ListSessionsResponse::new(sessions));
        }
        Err(e) => {
            tracing::error!("Failed to list sessions: {e}");
            let _ = responder.respond(ListSessionsResponse::new(vec![]));
        }
    }
    Ok(())
}

// ─── session/load handler ────────────────────────────────────────────────────

pub async fn handle_load_session(
    req: LoadSessionRequest,
    responder: agent_client_protocol::Responder<LoadSessionResponse>,
    conn: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let thread_id_str = req.session_id.0.as_ref().to_string();
    let cwd = req.cwd.to_string_lossy().to_string();
    let session_id_acp = req.session_id.clone();

    tracing::info!(thread_id = %thread_id_str, "ACP session/load request");

    // 加载线程历史
    let thread_id = peri_agent::thread::ThreadId::from(thread_id_str.clone());
    let messages = match mgr().load_thread_messages(&thread_id).await {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::error!(error = %e, "Failed to load session");
            let _ = responder.respond(LoadSessionResponse::new());
            return Ok(());
        }
    };

    // 创建 AcpSession 注册到 SessionManager
    let _ = mgr().new_session_with_id(&thread_id_str, &cwd).await;

    // 回放历史消息为 SessionNotification
    for msg in &messages {
        let updates = map_message_to_updates(msg);
        for update in updates {
            let notif = SessionNotification::new(session_id_acp.clone(), update);
            let _ = conn.send_notification(notif);
        }
    }

    tracing::info!(
        msg_count = messages.len(),
        "ACP session loaded and replayed"
    );

    let resp = mgr()
        .get_session(&thread_id_str)
        .map(|s| fill_load_session_resp(&s, LoadSessionResponse::new()))
        .unwrap_or_default();

    let _ = responder.respond(resp);

    // Send available commands after session load
    send_available_commands(&conn, &session_id_acp);

    Ok(())
}

// ─── session/resume handler ──────────────────────────────────────────────────

pub async fn handle_resume_session(
    req: agent_client_protocol::schema::ResumeSessionRequest,
    responder: agent_client_protocol::Responder<
        agent_client_protocol::schema::ResumeSessionResponse,
    >,
    conn: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let thread_id_str = req.session_id.0.as_ref().to_string();
    let cwd = req.cwd.to_string_lossy().to_string();

    tracing::info!(thread_id = %thread_id_str, "ACP session/resume request");

    // 创建 AcpSession 注册到 SessionManager（不回放消息）
    let _ = mgr().new_session_with_id(&thread_id_str, &cwd).await;

    let resp = mgr()
        .get_session(&thread_id_str)
        .map(|s| {
            let mut resp = agent_client_protocol::schema::ResumeSessionResponse::default();
            resp = resp.modes(Some(build_session_mode_state(&s)));
            resp = resp.config_options(Some(build_config_options(&s)));
            resp = resp.models(Some(build_session_model_state(&s, mgr().peri_config())));
            resp
        })
        .unwrap_or_default();

    let _ = responder.respond(resp);

    // Send available commands after session resume
    send_available_commands(&conn, &req.session_id);

    Ok(())
}

// ─── session/set_mode handler ────────────────────────────────────────────────

pub async fn handle_set_mode(
    req: SetSessionModeRequest,
    responder: agent_client_protocol::Responder<SetSessionModeResponse>,
    _conn: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let mode_id_str = req.mode_id.0.as_ref();
    let session_id_str = req.session_id.0.as_ref();

    let mode = match mode_id_str {
        "bypass" => PermissionMode::Bypass,
        "default" => PermissionMode::Default,
        "acceptEdits" => PermissionMode::AcceptEdit,
        "dontAsk" => PermissionMode::DontAsk,
        "auto" => PermissionMode::AutoMode,
        other => {
            tracing::warn!(mode_id = other, "Unknown mode, ignoring");
            let _ = responder.respond(SetSessionModeResponse::default());
            return Ok(());
        }
    };

    if let Some(session) = mgr().get_session(session_id_str) {
        session.permission_mode.store(mode);
        tracing::info!(session_id_str, mode_id_str, "Session mode changed");
    } else {
        tracing::warn!(session_id_str, "Session not found for set_mode");
    }

    // 发送 CurrentModeUpdate 通知，使客户端感知模式变更
    let _ = _conn.send_notification(SessionNotification::new(
        req.session_id.clone(),
        SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(req.mode_id.clone())),
    ));

    let _ = responder.respond(SetSessionModeResponse::default());
    Ok(())
}

// ─── session/set_model handler ────────────────────────────────────────────────

pub async fn handle_set_model(
    req: SetSessionModelRequest,
    responder: agent_client_protocol::Responder<SetSessionModelResponse>,
    _conn: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let model_id = req.model_id.0.as_ref().to_string();
    let session_id_str = req.session_id.0.as_ref();

    if let Some(mut session) = mgr().inner_sessions().get_mut(session_id_str) {
        session.model_alias = model_id.clone();
        tracing::info!(
            session_id = session_id_str,
            model_id,
            "Session model changed"
        );
    } else {
        tracing::warn!(
            session_id = session_id_str,
            "Session not found for set_model"
        );
    }

    // 发送 ConfigOptionUpdate 通知，使客户端感知模型变更
    let config_options = mgr()
        .get_session(session_id_str)
        .map(|s| build_config_options(&s))
        .unwrap_or_default();
    let _ = _conn.send_notification(SessionNotification::new(
        req.session_id.clone(),
        SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(config_options)),
    ));

    let _ = responder.respond(SetSessionModelResponse::default());
    Ok(())
}

// ─── session/set_config_option handler ───────────────────────────────────────

pub async fn handle_set_config_option(
    req: SetSessionConfigOptionRequest,
    responder: agent_client_protocol::Responder<SetSessionConfigOptionResponse>,
    _conn: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let session_id = req.session_id.0.as_ref();
    let config_id = req.config_id.0.as_ref();

    // 提取 value
    let value_id = match &req.value {
        agent_client_protocol::schema::SessionConfigOptionValue::ValueId { value } => {
            value.0.as_ref()
        }
        _ => {
            let _ = responder.respond(SetSessionConfigOptionResponse::new(vec![]));
            return Ok(());
        }
    };

    if let Some(mut session) = mgr().inner_sessions().get_mut(session_id) {
        match config_id {
            "mode" => {
                let mode = match value_id {
                    "bypass" => PermissionMode::Bypass,
                    "default" => PermissionMode::Default,
                    "acceptEdits" => PermissionMode::AcceptEdit,
                    "dontAsk" => PermissionMode::DontAsk,
                    "auto" => PermissionMode::AutoMode,
                    other => {
                        tracing::warn!(mode_id = other, "Unknown mode in config_option");
                        drop(session);
                        let _ = responder.respond(SetSessionConfigOptionResponse::new(vec![]));
                        return Ok(());
                    }
                };
                session.permission_mode.store(mode);
                tracing::info!(
                    session_id,
                    mode_id = value_id,
                    "Session mode changed via config_option"
                );
            }
            "model" => {
                session.model_alias = value_id.to_string();
                tracing::info!(
                    session_id,
                    model_id = value_id,
                    "Session model changed via config_option"
                );
            }
            "thinking_effort" => {
                let thinking =
                    session
                        .thinking
                        .get_or_insert_with(|| crate::config::ThinkingConfig {
                            enabled: true,
                            budget_tokens: 8000,
                            effort: "high".to_string(),
                            max_tokens: 32000,
                        });
                thinking.effort = value_id.to_string();
                tracing::info!(session_id, effort = value_id, "Thinking effort changed");
            }
            other => {
                tracing::warn!(config_id = other, "Unknown config option");
                drop(session);
                let _ = responder.respond(SetSessionConfigOptionResponse::new(vec![]));
                return Ok(());
            }
        }
    }

    // 构建更新后的 config_options
    let config_options = mgr()
        .get_session(session_id)
        .map(|s| build_config_options(&s))
        .unwrap_or_default();

    // 发送 ConfigOptionUpdate 通知，使客户端感知配置变更
    let _ = _conn.send_notification(SessionNotification::new(
        req.session_id.clone(),
        SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(config_options.clone())),
    ));

    // 如果是 mode 变更，额外发送 CurrentModeUpdate 通知
    if config_id == "mode" {
        let _ = _conn.send_notification(SessionNotification::new(
            req.session_id.clone(),
            SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(
                agent_client_protocol::schema::SessionModeId::new(value_id),
            )),
        ));
    }

    let _ = responder.respond(SetSessionConfigOptionResponse::new(config_options));
    Ok(())
}

// ─── session/fork handler ────────────────────────────────────────────────────

pub async fn handle_fork_session(
    req: ForkSessionRequest,
    responder: agent_client_protocol::Responder<ForkSessionResponse>,
    _conn: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let parent_id = req.session_id.0.as_ref();
    let cwd = req.cwd.to_string_lossy().to_string();

    // 从父 session 继承设置
    let (provider_id, model_alias, thinking) = mgr()
        .get_session(parent_id)
        .map(|s| {
            (
                s.provider_id.clone(),
                s.model_alias.clone(),
                s.thinking.clone(),
            )
        })
        .unwrap_or_else(|| {
            (
                mgr().peri_config().config.active_provider_id.clone(),
                mgr().peri_config().config.active_alias.clone(),
                mgr().peri_config().config.thinking.clone(),
            )
        });

    // 创建新 session
    match mgr()
        .new_session_with_settings(&cwd, provider_id, model_alias, thinking)
        .await
    {
        Ok((new_session_id, _)) => {
            let resp = mgr()
                .get_session(&new_session_id)
                .map(|s| {
                    let mut resp = ForkSessionResponse::new(new_session_id.clone());
                    resp = resp.modes(Some(build_session_mode_state(&s)));
                    resp = resp.config_options(Some(build_config_options(&s)));
                    resp = resp.models(Some(build_session_model_state(&s, mgr().peri_config())));
                    resp
                })
                .unwrap_or_else(|| ForkSessionResponse::new(new_session_id.clone()));

            let _ = responder.respond(resp);
            tracing::info!(parent_id, new_session_id = %new_session_id, "Session forked");
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to fork session");
            let _ = responder.respond(ForkSessionResponse::new(""));
        }
    }

    Ok(())
}
