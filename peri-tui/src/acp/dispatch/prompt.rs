use std::sync::Arc;

use agent_client_protocol::schema::{
    ContentBlock, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus, PromptRequest,
    PromptResponse, SessionNotification, SessionUpdate, StopReason,
};
use agent_client_protocol::{Client, ConnectionTo};
use peri_agent::agent::events::{AgentEvent as ExecutorEvent, FnEventHandler};
use peri_agent::agent::react::AgentInput;
use peri_agent::agent::state::AgentState;
use peri_agent::agent::AgentCancellationToken;
use peri_middlewares::tools::{TodoItem, TodoStatus};

use crate::app::agent::LlmProvider;

use super::super::{agent_assembler, broker::AcpInteractionBroker, event_mapper};
use super::mgr;

pub async fn handle_prompt(
    req: PromptRequest,
    responder: agent_client_protocol::Responder<PromptResponse>,
    conn: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let session_id_str = req.session_id.0.clone();
    let session_id_acp = req.session_id.clone();

    // 从 prompt 中提取文本
    let user_text: String = req
        .prompt
        .iter()
        .filter_map(|block| {
            if let ContentBlock::Text(tc) = block {
                Some(tc.text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if user_text.is_empty() {
        let _ = responder.respond(PromptResponse::new(StopReason::EndTurn));
        return Ok(());
    }

    // 获取 session 元数据
    let (
        thread_id,
        cwd,
        cancel_token,
        session_model_alias,
        session_permission_mode,
        _session_thinking,
    ) = {
        match mgr().get_session(&session_id_str) {
            Some(s) => (
                s.thread_id.clone(),
                s.cwd.clone(),
                s.cancel_token.clone(),
                s.model_alias.clone(),
                s.permission_mode.clone(),
                s.thinking.clone(),
            ),
            None => {
                tracing::warn!(session_id = %session_id_str, "Session not found for prompt");
                let _ = responder.respond(PromptResponse::new(StopReason::EndTurn));
                return Ok(());
            }
        }
    };

    tracing::info!(session_id = %session_id_str, text_len = user_text.len(), "ACP prompt received");

    // 从 session 级 model_alias 构建 LlmProvider
    let provider = LlmProvider::from_config_for_alias(mgr().peri_config(), &session_model_alias)
        .unwrap_or_else(|| mgr().provider().clone());

    let mgr_peri_config = mgr().peri_config().clone();
    let mgr_thread_store = mgr().thread_store().clone();

    // 将 Responder 和 conn 移入 spawned task，避免阻塞事件循环
    tokio::spawn(async move {
        // 加载线程历史
        let history = match mgr().load_thread_messages(&thread_id).await {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %e, "Failed to load thread history");
                let _ = responder.respond(PromptResponse::new(StopReason::EndTurn));
                return;
            }
        };

        // 构建系统提示词
        let features = crate::prompt::PromptFeatures::detect();
        let system_prompt = crate::prompt::build_system_prompt(None, &cwd, features, &[]);

        // 创建 CancellationToken（关联 session cancel_token）
        let cancel = AgentCancellationToken::new();
        let cancel_for_link = cancel.clone();
        let cancel_token_for_link = cancel_token.clone();
        tokio::spawn(async move {
            cancel_token_for_link.cancelled().await;
            cancel_for_link.cancel();
        });

        // 事件处理器：ExecutorEvent → SessionUpdate → SessionNotification → conn.send_notification()
        let conn_for_handler = conn.clone();
        let sid_for_handler = session_id_acp.clone();
        let context_window = {
            let mut cw = provider.context_window();
            if mgr_peri_config.config.context_1m.unwrap_or(false) {
                cw = 1_000_000;
            }
            cw
        };
        let handler: Arc<dyn peri_agent::agent::events::AgentEventHandler> =
            Arc::new(FnEventHandler(move |event: ExecutorEvent| {
                let updates = event_mapper::map_executor_to_updates(&event, context_window);
                for update in updates {
                    let notif = SessionNotification::new(sid_for_handler.clone(), update);
                    let _ = conn_for_handler.send_notification(notif);
                }
            }));

        // 创建 ACP 权限桥接 broker + 权限转发循环
        let (perm_tx, perm_rx) = tokio::sync::mpsc::channel(16);
        let broker = Arc::new(AcpInteractionBroker::new(perm_tx));

        // 权限转发：perm_rx → RequestPermissionRequest → conn.send_request() → map → response_tx
        let conn_for_perm = conn.clone();
        let sid_for_perm = session_id_acp.clone();
        let mgr_for_perm = mgr().clone();
        tokio::spawn(async move {
            super::super::broker::permission_forwarding_loop(
                perm_rx,
                conn_for_perm,
                sid_for_perm,
                mgr_for_perm,
            )
            .await;
        });

        // 组装 Agent
        let config = agent_assembler::AgentAssembleConfig {
            provider,
            cwd: cwd.clone(),
            system_prompt,
            broker,
            permission_mode: session_permission_mode,
            peri_config: mgr_peri_config,
            event_handler: handler,
            cancel: cancel.clone(),
            cron_scheduler: None,
            agent_overrides: mgr().agent_overrides().cloned(),
            preload_skills: Vec::new(),
            session_id: Some(session_id_str.to_string()),
        };
        let (executor, mut todo_rx) = agent_assembler::assemble_agent(config);

        // 转发 Todo 更新为 SessionUpdate
        let conn_for_todo = conn.clone();
        let sid_for_todo = session_id_acp.clone();
        tokio::spawn(async move {
            while let Some(todos) = todo_rx.recv().await {
                let entries: Vec<_> = todos
                    .iter()
                    .map(|t: &TodoItem| {
                        PlanEntry::new(
                            t.content.clone(),
                            PlanEntryPriority::Medium,
                            match t.status {
                                TodoStatus::Completed => PlanEntryStatus::Completed,
                                TodoStatus::InProgress => PlanEntryStatus::InProgress,
                                TodoStatus::Pending => PlanEntryStatus::Pending,
                            },
                        )
                    })
                    .collect();
                let notif = SessionNotification::new(
                    sid_for_todo.clone(),
                    SessionUpdate::Plan(Plan::new(entries)),
                );
                let _ = conn_for_todo.send_notification(notif);
            }
        });

        // 创建 AgentState（带历史 + 持久化）
        let history_len = history.len();
        let mut state =
            AgentState::with_messages(cwd, history).with_persistence(mgr_thread_store, thread_id);

        let input = AgentInput::text(user_text);
        let result = executor.execute(input, &mut state, Some(cancel)).await;

        // new_msgs 通过 AgentState 的 with_persistence 已自动持久化
        tracing::info!(
            new_msgs = state.into_messages().len().saturating_sub(history_len),
            "ACP prompt execution finished"
        );

        let stop_reason = match &result {
            Ok(_) => StopReason::EndTurn,
            Err(peri_agent::error::AgentError::Interrupted) => StopReason::Cancelled,
            Err(peri_agent::error::AgentError::MaxIterationsExceeded(_)) => {
                StopReason::MaxTurnRequests
            }
            Err(e) => {
                tracing::error!(error = %e, "ACP prompt execution error");
                StopReason::EndTurn
            }
        };

        let _ = responder.respond(PromptResponse::new(stop_reason));
    });

    Ok(())
}
