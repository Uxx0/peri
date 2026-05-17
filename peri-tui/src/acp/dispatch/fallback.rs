use agent_client_protocol::schema::ClientNotification;
use agent_client_protocol::{Client, ConnectionTo, Dispatch, Handled};

use super::mgr;

pub async fn handle_dispatch(
    msg: Dispatch,
    _conn: ConnectionTo<Client>,
) -> Result<Handled<Dispatch>, agent_client_protocol::Error> {
    match msg {
        Dispatch::Notification(notif) => {
            // Try to parse as typed ClientNotification first
            if let Ok(typed) = serde_json::from_value::<ClientNotification>(notif.params().clone())
            {
                match typed {
                    ClientNotification::CancelNotification(cancel) => {
                        let session_id = cancel.session_id.0.as_ref();
                        mgr().cancel_session(session_id);
                        tracing::info!(session_id = %session_id, "ACP session cancelled");
                        return Ok(Handled::Yes);
                    }
                    _ => return Ok(Handled::Yes),
                }
            }

            // Check if this is a $/cancel_request notification
            if notif.method() == "$/cancel_request" {
                if let Some(params) = notif.params().as_object() {
                    if let Some(request_id) = params.get("requestId") {
                        let rid =
                            serde_json::from_value::<agent_client_protocol::schema::RequestId>(
                                request_id.clone(),
                            )
                            .unwrap_or(agent_client_protocol::schema::RequestId::Null);
                        mgr().cancel_pending_request(&rid);
                        tracing::info!(?request_id, "$/cancel_request handled in dispatch");
                        return Ok(Handled::Yes);
                    }
                }
                tracing::warn!("$/cancel_request received with no requestId");
                return Ok(Handled::Yes);
            }

            Ok(Handled::Yes)
        }
        // 未匹配的请求传递给下一个 handler
        other => Ok(Handled::No {
            message: other,
            retry: false,
        }),
    }
}
