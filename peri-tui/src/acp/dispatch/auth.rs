use agent_client_protocol::schema::{
    AuthenticateRequest, AuthenticateResponse, LogoutRequest, LogoutResponse,
};
use agent_client_protocol::{Client, ConnectionTo};

pub async fn handle_authenticate(
    req: AuthenticateRequest,
    responder: agent_client_protocol::Responder<AuthenticateResponse>,
    _conn: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    tracing::info!(method_id = %req.method_id, "ACP authenticate request");
    let _ = responder.respond(AuthenticateResponse::new());
    Ok(())
}

pub async fn handle_logout(
    _req: LogoutRequest,
    responder: agent_client_protocol::Responder<LogoutResponse>,
    _conn: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    tracing::info!("ACP logout request");
    let _ = responder.respond(LogoutResponse::new());
    Ok(())
}
