use crate::types::*;
use rmcp::model::{CustomNotification, ServerNotification};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Telegram Bot client for long-polling and sending messages
pub struct TelegramClient {
    http: reqwest::Client,
    base_url: String,
    update_offset: Arc<std::sync::atomic::AtomicI64>,
    chat_ids: Arc<std::sync::Mutex<Vec<i64>>>,
}

impl TelegramClient {
    pub fn new(token: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent("telegram-bridge/0.2")
                .build()
                .expect("Failed to create HTTP client"),
            base_url: format!("https://api.telegram.org/bot{}", token),
            update_offset: Arc::new(std::sync::atomic::AtomicI64::new(0)),
            chat_ids: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Start long-polling for Telegram updates.
    /// New messages are forwarded as MCP notifications through `notif_tx`.
    pub async fn start_polling(
        &self,
        cancel: tokio_util::sync::CancellationToken,
        notif_tx: mpsc::UnboundedSender<ServerNotification>,
    ) {
        tracing::info!("Starting Telegram polling...");

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::info!("Telegram polling cancelled");
                    break;
                }
                result = self.get_updates() => {
                    match result {
                        Ok(updates) => {
                            for update in updates {
                                self.update_offset
                                    .store(update.update_id + 1, std::sync::atomic::Ordering::SeqCst);

                                if let Some(msg) = update.message {
                                    if let Some(text) = msg.text.clone() {
                                        self.record_chat(msg.chat.id);

                                        let channel_msg = ChannelMessage {
                                            source: "telegram-bridge".to_string(),
                                            chat_id: msg.chat.id.to_string(),
                                            text,
                                        };

                                        // Send MCP notification: notifications/claude/channel
                                        let notif = ServerNotification::CustomNotification(
                                            CustomNotification::new(
                                                "notifications/claude/channel",
                                                Some(serde_json::to_value(&channel_msg)
                                                    .unwrap_or_default()),
                                            ),
                                        );

                                        if let Err(e) = notif_tx.send(notif) {
                                            tracing::error!("Failed to send channel notification: {e}");
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("Error fetching updates: {e}");
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
            }
        }
    }

    async fn get_updates(&self) -> anyhow::Result<Vec<TelegramUpdate>> {
        let offset = self.update_offset.load(std::sync::atomic::Ordering::SeqCst);
        let url = format!(
            "{}/getUpdates?offset={}&timeout=30&allowed_updates={}",
            self.base_url,
            offset,
            serde_json::json!(["message"])
        );

        let resp: TelegramResponse<Vec<TelegramUpdate>> = self
            .http
            .get(&url)
            .send()
            .await?
            .json()
            .await?;

        if resp.ok {
            Ok(resp.result.unwrap_or_default())
        } else {
            Err(anyhow::anyhow!(
                "Telegram API error: {}",
                resp.description.unwrap_or_default()
            ))
        }
    }

    /// Send a message to a Telegram chat
    pub async fn send_message(&self, chat_id: i64, text: &str) -> anyhow::Result<()> {
        let url = format!("{}/sendMessage", self.base_url);
        let body = SendMessageRequest {
            chat_id,
            text: text.to_string(),
            parse_mode: Some("HTML".to_string()),
        };

        let resp: TelegramResponse<serde_json::Value> = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;

        if resp.ok {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "Failed to send message: {}",
                resp.description.unwrap_or_default()
            ))
        }
    }

    /// Get list of known chat IDs
    pub fn get_chats(&self) -> Vec<(i64, String)> {
        self.chat_ids
            .lock()
            .unwrap()
            .iter()
            .map(|id| (*id, format!("Chat {}", id)))
            .collect()
    }

    fn record_chat(&self, chat_id: i64) {
        let mut chats = self.chat_ids.lock().unwrap();
        if !chats.contains(&chat_id) {
            chats.push(chat_id);
        }
    }
}
