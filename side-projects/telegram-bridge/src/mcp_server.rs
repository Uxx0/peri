use crate::telegram::TelegramClient;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, CustomNotification, ErrorCode, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{MaybeSendFuture, NotificationContext, RequestContext, RoleServer};
use rmcp::ErrorData as McpError;
use serde_json::json;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

/// Helper to convert a serde_json::Value (object) into an Arc<JsonObject>
fn schema(value: serde_json::Value) -> Arc<serde_json::Map<String, serde_json::Value>> {
    match value {
        serde_json::Value::Object(map) => Arc::new(map),
        _ => panic!("schema must be a JSON object"),
    }
}

/// MCP server handler for the Telegram bridge
pub struct TelegramMcpServer {
    telegram: Arc<TelegramClient>,
}

impl TelegramMcpServer {
    pub fn new(telegram: Arc<TelegramClient>) -> Self {
        Self { telegram }
    }
}

impl ServerHandler for TelegramMcpServer {
    fn get_info(&self) -> ServerInfo {
        // Declare channel capability via experimental field
        let mut experimental = BTreeMap::new();
        experimental.insert(
            "claude/channel".to_string(),
            serde_json::Map::new(), // empty object
        );

        let capabilities = ServerCapabilities::builder()
            .enable_experimental_with(experimental)
            .build();

        ServerInfo::new(capabilities).with_server_info(rmcp::model::Implementation::new(
            "telegram-bridge",
            env!("CARGO_PKG_VERSION"),
        ))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + MaybeSendFuture + '_ {
        async move {
            let telegram_send = Tool::new(
                "telegram__send",
                "Send a message to a Telegram chat. The message is sent as HTML formatted text.",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "chat_id": {
                            "type": "integer",
                            "description": "Telegram chat ID to send the message to"
                        },
                        "text": {
                            "type": "string",
                            "description": "Message text (HTML formatted)"
                        }
                    },
                    "required": ["chat_id", "text"]
                })),
            );

            let telegram_list_chats = Tool::new(
                "telegram__list_chats",
                "List known Telegram chats that have interacted with this bot.",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "filter": {
                            "type": "string",
                            "description": "Optional filter text"
                        }
                    }
                })),
            );

            let telegram_server_info = Tool::new(
                "telegram__server_info",
                "Get information about the Telegram bridge server.",
                schema(json!({
                    "type": "object",
                    "properties": {
                        "detail": {
                            "type": "string",
                            "description": "Optional detail level"
                        }
                    }
                })),
            );

            Ok(ListToolsResult::with_all_items(vec![
                telegram_send,
                telegram_list_chats,
                telegram_server_info,
            ]))
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + MaybeSendFuture + '_ {
        async move {
            match request.name.as_ref() {
                "telegram__send" => {
                    let chat_id = request
                        .arguments
                        .as_ref()
                        .and_then(|a| a.get("chat_id"))
                        .and_then(|v| v.as_i64())
                        .ok_or_else(|| {
                            McpError::invalid_params("Missing or invalid 'chat_id'", None)
                        })?;
                    let text = request
                        .arguments
                        .as_ref()
                        .and_then(|a| a.get("text"))
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            McpError::invalid_params("Missing or invalid 'text'", None)
                        })?;

                    self.telegram
                        .send_message(chat_id, text)
                        .await
                        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "Message sent to chat {} (text length: {})",
                        chat_id,
                        text.len()
                    ))]))
                }
                "telegram__list_chats" => {
                    let chats = self.telegram.get_chats();
                    let content = if chats.is_empty() {
                        "No known chats yet. Send a message to the bot first.".to_string()
                    } else {
                        let chat_list: Vec<String> = chats
                            .iter()
                            .map(|(id, label)| format!("- {} (ID: {})", label, id))
                            .collect();
                        format!("Known chats:\n{}", chat_list.join("\n"))
                    };

                    Ok(CallToolResult::success(vec![Content::text(content)]))
                }
                "telegram__server_info" => {
                    let info = ServerInfoResponse {
                        name: "telegram-bridge".to_string(),
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        channels: vec!["telegram".to_string()],
                    };

                    Ok(CallToolResult::success(vec![Content::text(
                        serde_json::to_string_pretty(&info).unwrap_or_default(),
                    )]))
                }
                name => Err(McpError::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    format!("Unknown tool: {name}"),
                    None,
                )),
            }
        }
    }

    fn on_custom_notification(
        &self,
        _notification: CustomNotification,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        // Handle incoming custom notifications (e.g., permission responses from peri)
        async move {
            // Not currently used - permission handling is future work
        }
    }
}

/// Server info response structure
#[derive(serde::Serialize)]
struct ServerInfoResponse {
    name: String,
    version: String,
    channels: Vec<String>,
}
