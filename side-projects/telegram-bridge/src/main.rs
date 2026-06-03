mod mcp_server;
mod telegram;
mod types;

use mcp_server::TelegramMcpServer;
use rmcp::model::ServerNotification;
use rmcp::ServiceExt;
use std::sync::Arc;
use telegram::TelegramClient;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "telegram_bridge=info".into()),
        )
        .init();

    // Get bot token from environment
    let bot_token = std::env::var("TELEGRAM_BOT_TOKEN")
        .map_err(|_| anyhow::anyhow!("TELEGRAM_BOT_TOKEN environment variable not set"))?;

    tracing::info!("Starting Telegram bridge...");

    // Channel for forwarding MCP notifications from background tasks → MCP peer
    let (notif_tx, notif_rx) = mpsc::unbounded_channel::<ServerNotification>();

    // Create Telegram client
    let telegram = Arc::new(TelegramClient::new(&bot_token));

    // Start Telegram polling in background
    let poll_telegram = telegram.clone();
    let cancel_token = CancellationToken::new();
    let cancel_poll = cancel_token.clone();
    let poll_notif_tx = notif_tx.clone();
    tokio::spawn(async move {
        poll_telegram
            .start_polling(cancel_poll, poll_notif_tx)
            .await;
    });

    // Create the MCP server handler (no notification sender needed inside the handler)
    let handler = TelegramMcpServer::new(telegram.clone());

    // Start MCP server on stdio transport
    tracing::info!("Starting MCP server on stdio...");
    let service = handler
        .serve(rmcp::transport::io::stdio())
        .await
        .map_err(|e| anyhow::anyhow!("Failed to start MCP server: {e}"))?;

    tracing::info!("Telegram bridge is running.");

    // Spawn a task that forwards MCP notifications through the peer.
    // The service is moved into this task; dropping the service
    // (when the notification channel closes) triggers clean MCP shutdown.
    let service_task = tokio::spawn(async move {
        let mut rx = notif_rx;
        while let Some(notification) = rx.recv().await {
            tracing::debug!("Forwarding MCP notification to client");
            if let Err(e) = service.send_notification(notification).await {
                tracing::error!("Failed to send notification: {e}");
                break;
            }
        }
        tracing::info!("Notification forwarding ended, MCP service will shut down");
    });

    // Wait for Ctrl+C to shut down
    tokio::signal::ctrl_c().await?;
    tracing::info!("Received Ctrl+C, shutting down...");
    cancel_token.cancel();

    // Wait for the service task to finish (it will exit when the notification
    // channel closes after the polling task stops)
    let _ = service_task.await;

    tracing::info!("Telegram bridge shut down.");
    Ok(())
}
