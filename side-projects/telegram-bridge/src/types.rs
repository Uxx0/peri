use serde::{Deserialize, Serialize};

/// Telegram Update from Bot API
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramUpdate {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<TelegramMessage>,
}

/// Telegram Message
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramMessage {
    pub message_id: i64,
    pub chat: TelegramChat,
    pub text: Option<String>,
    pub from: Option<TelegramUser>,
}

/// Telegram Chat
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TelegramChat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
    pub title: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub username: Option<String>,
}

/// Telegram User
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramUser {
    pub id: i64,
    pub is_bot: bool,
    pub first_name: String,
    pub username: Option<String>,
}

/// Outgoing message to Telegram
#[derive(Debug, Serialize)]
pub struct SendMessageRequest {
    pub chat_id: i64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<String>,
}

/// Response from Telegram API
#[derive(Debug, Deserialize)]
pub struct TelegramResponse<T> {
    pub ok: bool,
    pub description: Option<String>,
    pub result: Option<T>,
}

/// MCP channel notification content (must match peri's ChannelNotification)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub source: String,
    pub chat_id: String,
    pub text: String,
}

/// MCP permission request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub source: String,
    pub user_id: String,
    pub tool_call: serde_json::Value,
    pub request_id: String,
}

/// MCP permission response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponse {
    pub source: String,
    pub request_id: String,
    pub approved: bool,
}
