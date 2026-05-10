use async_trait::async_trait;
use serde_json::{json, Value};

use super::BaseModel;
use crate::agent::react::{ReactLLM, Reasoning, ToolCall};
use crate::error::{AgentError, AgentResult};
use crate::llm::types::{LlmRequest, LlmResponse, StopReason};
use crate::messages::{BaseMessage, ContentBlock, ImageSource, MessageContent, ToolCallRequest};
use crate::tools::BaseTool;

/// ChatOpenAI - OpenAI 兼容 API 的 LLM 实现
pub struct ChatOpenAI {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    /// o1/o3 系列推理强度："low" | "medium" | "high"
    /// 设置后请求体加 `reasoning_effort` 字段，同时移除 temperature
    pub reasoning_effort: Option<String>,
    /// 是否在请求体中发送 `thinking: { type: "enabled" }`（deepseek-v4-pro 等）
    pub thinking_enabled: bool,
    /// 是否在 content 中回传 `thinking` 类型的 Reasoning 块。
    /// 仅 deepseek-v4-pro 等明确支持的模型开启，其他 provider 不支持会报 400。
    pub supports_thinking_content: bool,
    client: reqwest::Client,
}

impl ChatOpenAI {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        let model = model.into();
        Self {
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".to_string(),
            reasoning_effort: None,
            thinking_enabled: false,
            supports_thinking_content: Self::detect_thinking_content_support(&model),
            model,
            client: reqwest::Client::new(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// 开启 reasoning effort（o1/o3 系列）
    /// `effort`: "low" | "medium" | "high"
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    /// 开启 DeepSeek thinking 模式（deepseek-v4-pro 等）
    ///
    /// 请求体中添加 `"thinking": {"type": "enabled"}`，API 会返回 `reasoning_content` 字段。
    /// 注意：`supports_thinking_content` 由构造函数根据模型名自动检测，此方法不修改它。
    /// 只有 deepseek-v4 系列支持 content 数组中的 `thinking` 块，其他模型只支持
    /// 顶层 `reasoning_content` 字段回传。
    pub fn with_thinking_enabled(mut self) -> Self {
        self.thinking_enabled = true;
        self
    }

    /// 手动控制是否在 content 中回传 `thinking` 类型的 Reasoning 块
    pub fn with_thinking_content(mut self, enabled: bool) -> Self {
        self.supports_thinking_content = enabled;
        self
    }

    /// 根据模型名检测是否支持 content 中的 `thinking` 类型
    fn detect_thinking_content_support(model: &str) -> bool {
        let m = model.to_lowercase();
        // deepseek-v4-pro 等要求回传 thinking content
        m.contains("deepseek-v4")
    }

    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("OPENAI_API_KEY").ok()?;
        let base_url = std::env::var("OPENAI_API_BASE")
            .or_else(|_| std::env::var("OPENAI_BASE_URL"))
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let model = std::env::var("OPENAI_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "gpt-4o".to_string());
        Some(Self::new(api_key, model).with_base_url(base_url))
    }

    /// 模型的上下文窗口大小（token 数），作为固有方法提供给 BaseModel 和 ReactLLM trait
    fn context_window_inner(&self) -> u32 {
        let model = self.model.to_lowercase();
        if model.contains("gpt-4") {
            return 128_000;
        }
        if model.starts_with("o1") || model.starts_with("o3") {
            return 200_000;
        }
        if model.contains("gpt-3.5") {
            return 16_385;
        }
        if model.starts_with("deepseek") {
            return 128_000;
        }
        200_000
    }

    // ─── MessageContent → OpenAI content ──────────────────────────────────────

    /// 将 MessageContent 序列化为 OpenAI content 字段
    ///
    /// - `Text(s)` → 字符串
    /// - `Blocks(v)` → array of content parts
    /// - `Raw(v)` → 透传
    pub(crate) fn content_to_openai(
        content: &MessageContent,
        supports_thinking_content: bool,
    ) -> Value {
        match content {
            MessageContent::Text(s) => json!(s),
            MessageContent::Blocks(blocks) => {
                let parts: Vec<Value> = blocks
                    .iter()
                    .filter_map(|b| Self::block_to_openai_part(b, supports_thinking_content))
                    .collect();
                if parts.is_empty() {
                    json!("")
                } else {
                    Value::Array(parts)
                }
            }
            MessageContent::Raw(values) => Value::Array(values.clone()),
        }
    }

    fn block_to_openai_part(
        block: &ContentBlock,
        supports_thinking_content: bool,
    ) -> Option<Value> {
        match block {
            ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
            ContentBlock::Image { source } => {
                let image_url = match source {
                    ImageSource::Url { url } => json!({ "url": url }),
                    ImageSource::Base64 { media_type, data } => {
                        json!({ "url": format!("data:{media_type};base64,{data}") })
                    }
                };
                Some(json!({ "type": "image_url", "image_url": image_url }))
            }
            // ToolUse / ToolResult 在 assistant / tool 角色消息中处理，此处跳过
            ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. } => None,
            // Reasoning: 仅在 provider 支持 thinking content type 时回传
            ContentBlock::Reasoning { text, signature } if supports_thinking_content => {
                let mut obj = json!({ "type": "thinking", "thinking": text });
                if let Some(sig) = signature {
                    obj["signature"] = json!(sig);
                }
                Some(obj)
            }
            ContentBlock::Reasoning { .. } => None,
            // Document / Unknown 透传为 raw JSON（OpenAI 可能不支持，但透传保持兼容）
            ContentBlock::Document { source, title } => {
                let src = serde_json::to_value(source).unwrap_or_default();
                Some(json!({ "type": "document", "source": src, "title": title }))
            }
            ContentBlock::Unknown(v) => Some(v.clone()),
        }
    }

    /// 从 MessageContent 中提取所有 Reasoning block 的文本
    ///
    /// DeepSeek R1 要求将 reasoning_content 作为 assistant 消息的顶层字段回传。
    fn extract_reasoning_text(content: &MessageContent) -> Option<String> {
        match content {
            MessageContent::Blocks(blocks) => {
                let parts: Vec<&str> = blocks.iter().filter_map(|b| b.as_reasoning()).collect();
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join(""))
                }
            }
            _ => None,
        }
    }

    pub(crate) fn messages_to_json(&self, messages: &[BaseMessage]) -> Vec<Value> {
        // DeepSeek v4 thinking 模式：所有 assistant 消息必须包含 reasoning_content 字段
        let force_reasoning_content =
            self.thinking_enabled || self.model.to_lowercase().contains("deepseek");

        // 单次遍历：收集 System 消息并处理其他消息
        let mut system_parts: Vec<String> = Vec::new();
        let mut result: Vec<Value> = Vec::new();

        for m in messages {
            match m {
                BaseMessage::System { content, .. } => {
                    let t = content.text_content();
                    if !t.trim().is_empty() {
                        system_parts.push(t);
                    }
                }
                BaseMessage::Human { content, .. } => {
                    result.push(
                        json!({ "role": "user", "content": Self::content_to_openai(content, self.supports_thinking_content) }),
                    );
                }
                BaseMessage::Ai {
                    content,
                    tool_calls,
                    ..
                } => {
                    // 提取 reasoning 文本（DeepSeek R1 要求回传 reasoning_content 顶层字段）
                    let reasoning_text = Self::extract_reasoning_text(content);
                    let serialized_content =
                        Self::content_to_openai(content, self.supports_thinking_content);

                    if tool_calls.is_empty() {
                        let mut msg = json!({ "role": "assistant", "content": serialized_content });
                        if let Some(rt) = &reasoning_text {
                            msg["reasoning_content"] = json!(rt);
                        } else if force_reasoning_content {
                            msg["reasoning_content"] = json!("");
                        }
                        result.push(msg);
                    } else {
                        let tcs: Vec<Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": tc.arguments.to_string()
                                    }
                                })
                            })
                            .collect();
                        let mut msg = json!({
                            "role": "assistant",
                            "content": serialized_content,
                            "tool_calls": tcs
                        });
                        if let Some(rt) = &reasoning_text {
                            msg["reasoning_content"] = json!(rt);
                        } else if force_reasoning_content {
                            msg["reasoning_content"] = json!("");
                        }
                        result.push(msg);
                    }
                }
                BaseMessage::Tool {
                    tool_call_id,
                    content,
                    ..
                } => {
                    result.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": Self::content_to_openai(content, self.supports_thinking_content)
                    }));
                }
            }
        }

        if !system_parts.is_empty() {
            result.insert(
                0,
                json!({ "role": "system", "content": system_parts.join("\n\n") }),
            );
        }

        result
    }

    // ─── 响应 → BaseMessage ───────────────────────────────────────────────────

    /// 将 OpenAI 响应解析为 BaseMessage（含 reasoning block）
    ///
    /// 支持 `o1/o3/deepseek-r1` 格式：
    /// - `message.reasoning_content` → `ContentBlock::Reasoning`
    /// - `message.content` → `ContentBlock::Text`
    ///
    /// 支持 `deepseek-v4-pro` thinking 模式：
    /// - `message.content` 数组中的 `{"type": "thinking", "thinking": "..."}` → `ContentBlock::Reasoning`
    /// - `message.content` 数组中的 `{"type": "text", "text": "..."}` → `ContentBlock::Text`
    /// - `message.reasoning_content` 顶层字段（如存在）→ `ContentBlock::Reasoning`
    fn parse_assistant_message(assistant_msg: &Value, stop_reason: &StopReason) -> BaseMessage {
        // 检测 content 是字符串还是数组
        let content_val = &assistant_msg["content"];
        let is_array = content_val.is_array();

        let mut blocks: Vec<ContentBlock> = Vec::new();
        let mut text_parts: Vec<String> = Vec::new();

        // 1) reasoning_content 顶层字段（deepseek-r1、某些 OpenAI o 系列）
        let mut has_top_level_reasoning = false;
        if let Some(reasoning) = assistant_msg["reasoning_content"].as_str() {
            if !reasoning.is_empty() {
                blocks.push(ContentBlock::reasoning(reasoning));
                has_top_level_reasoning = true;
            }
        }

        if is_array {
            // content 是数组格式（deepseek-v4-pro thinking 模式等）
            if let Some(arr) = content_val.as_array() {
                for item in arr {
                    let item_type = item["type"].as_str().unwrap_or("");
                    match item_type {
                        "thinking"
                            // content 数组中的 thinking 块（deepseek-v4-pro）
                            // 如果顶层 reasoning_content 已存在，跳过避免重复
                            if !has_top_level_reasoning => {
                                if let Some(thinking_text) = item["thinking"].as_str() {
                                    if !thinking_text.is_empty() {
                                        blocks.push(ContentBlock::reasoning(thinking_text));
                                    }
                                }
                            }
                        "text" => {
                            if let Some(t) = item["text"].as_str() {
                                if !t.is_empty() {
                                    text_parts.push(t.to_string());
                                }
                            }
                        }
                        // 其他类型（image_url 等）暂不处理
                        _ => {}
                    }
                }
            }
        } else {
            // content 是字符串格式（传统 OpenAI / deepseek-r1）
            let content_str = content_val.as_str().unwrap_or("");
            if !content_str.is_empty() {
                text_parts.push(content_str.to_string());
            }
        }

        // 合并文本
        let content_str = text_parts.join("");

        // 添加文本 block
        if !content_str.is_empty() {
            blocks.push(ContentBlock::text(&content_str));
        }

        if *stop_reason == StopReason::ToolUse {
            // tool_calls 也提取为 ToolUse blocks + ToolCallRequest
            let tool_calls: Vec<ToolCallRequest> = assistant_msg["tool_calls"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(|tc| {
                    let id = tc["id"].as_str()?;
                    let name = tc["function"]["name"].as_str()?;
                    let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                    let arguments = match serde_json::from_str::<Value>(args_str) {
                        Ok(v) => v,
                        Err(_) => {
                            tracing::warn!(
                                tool = name,
                                raw_args = %args_str,
                                "OpenAI tool_call arguments JSON 解析失败，使用空对象"
                            );
                            serde_json::json!({"_raw_arguments": args_str})
                        }
                    };
                    blocks.push(ContentBlock::tool_use(id, name, arguments.clone()));
                    Some(ToolCallRequest::new(id, name, arguments))
                })
                .collect();

            let content = if blocks.len() == 1 && blocks[0].as_text().is_some() {
                // 没有 reasoning，只有文本 → 保持简单 Text
                MessageContent::text(content_str)
            } else if blocks.is_empty() {
                MessageContent::default()
            } else {
                MessageContent::Blocks(blocks)
            };

            BaseMessage::ai_with_tool_calls(content, tool_calls)
        } else if blocks.len() == 1 && blocks[0].as_text().is_some() {
            // 普通文本回复，保持简单形式
            BaseMessage::ai(content_str)
        } else if blocks.is_empty() {
            BaseMessage::ai("")
        } else {
            // 含 reasoning block（或其他 block）→ Blocks 形式
            BaseMessage::ai(MessageContent::Blocks(blocks))
        }
    }
}

#[async_trait]
impl BaseModel for ChatOpenAI {
    async fn invoke(&self, request: LlmRequest) -> AgentResult<LlmResponse> {
        let msg_count = request.messages.len();
        let start = std::time::Instant::now();

        let chat_url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let tools_json: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    }
                })
            })
            .collect();

        let mut messages = self.messages_to_json(&request.messages);

        // 验证消息序列不变量：每个 tool 消息前必须有 assistant with tool_calls
        for (i, msg) in messages.iter().enumerate() {
            if msg["role"] == "tool"
                && (i == 0 || {
                    let prev = &messages[i - 1];
                    prev["role"] != "assistant" || !prev["tool_calls"].is_array()
                })
            {
                tracing::error!(
                    position = i,
                    total = messages.len(),
                    prev = ?messages.get(i.saturating_sub(1)).map(|m| &m["role"]),
                    "消息序列不变量违反：tool 消息前缺少 assistant with tool_calls"
                );
            }
        }

        if let Some(base_system) = &request.system {
            if let Some(first) = messages.first_mut() {
                if first["role"] == "system" {
                    // 消息列表中已有 System（来自中间件，如 agent.md），追加基础提示词
                    let existing = first["content"].as_str().unwrap_or("");
                    first["content"] = json!(format!("{}\n\n{}", existing, base_system));
                } else {
                    messages.insert(0, json!({ "role": "system", "content": base_system }));
                }
            } else {
                messages.insert(0, json!({ "role": "system", "content": base_system }));
            }
        }

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false
        });

        if !tools_json.is_empty() {
            body["tools"] = Value::Array(tools_json);
            body["tool_choice"] = json!("auto");
        }

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }

        if let Some(ref effort) = self.reasoning_effort {
            // o1/o3 reasoning effort 模式：加 reasoning_effort，不设 temperature
            body["reasoning_effort"] = json!(effort);
        } else if let Some(temperature) = request.temperature {
            body["temperature"] = json!(temperature);
        }

        // DeepSeek thinking 模式（deepseek-v4-pro 等）
        if self.thinking_enabled {
            body["thinking"] = json!({ "type": "enabled" });
        }

        let resp = self
            .client
            .post(&chat_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                tracing::error!(
                    provider = "openai",
                    model = %self.model,
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    error = %e,
                    "LLM 网络请求失败"
                );
                AgentError::LlmError(e.to_string())
            })?;

        let status = resp.status();
        let resp_text = resp.text().await.map_err(|e| {
            tracing::error!(
                provider = "openai",
                model = %self.model,
                status = %status,
                elapsed_ms = start.elapsed().as_millis() as u64,
                error = %e,
                "LLM 读取响应体失败"
            );
            AgentError::LlmError(format!("读取响应体失败: {e}"))
        })?;
        let resp_json: Value = serde_json::from_str(&resp_text).map_err(|e| {
            tracing::error!(
                provider = "openai",
                model = %self.model,
                status = %status,
                elapsed_ms = start.elapsed().as_millis() as u64,
                error = %e,
                "LLM 响应解析失败"
            );
            AgentError::LlmError(format!(
                "解析响应失败: {e}\n原始响应({status}): {resp_text}"
            ))
        })?;

        if !status.is_success() {
            let msg = resp_json["error"]["message"]
                .as_str()
                .unwrap_or("未知错误")
                .to_string();
            let error_type = resp_json["error"]["type"].as_str().unwrap_or("unknown");
            let error_code = resp_json["error"]["code"].as_str().unwrap_or("");
            tracing::error!(
                provider = "openai",
                model = %self.model,
                status = %status,
                error_type,
                error_code,
                error_message = %msg,
                elapsed_ms = start.elapsed().as_millis() as u64,
                msg_count,
                "LLM API 错误"
            );
            return Err(AgentError::LlmHttpError {
                status: status.as_u16(),
                message: format!("API 错误 {status}: {msg}"),
            });
        }

        tracing::info!(
            provider = "openai",
            model = %self.model,
            status = %status,
            elapsed_ms = start.elapsed().as_millis() as u64,
            msg_count,
            input_tokens = resp_json["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            output_tokens = resp_json["usage"]["completion_tokens"].as_u64().unwrap_or(0),
            "LLM invoke completed"
        );

        let choice = &resp_json["choices"][0];
        let finish_reason = choice["finish_reason"].as_str().unwrap_or("stop");
        let stop_reason = StopReason::from_openai(finish_reason);
        let assistant_msg = &choice["message"];

        let message = Self::parse_assistant_message(assistant_msg, &stop_reason);

        let usage = {
            let input = resp_json["usage"]["prompt_tokens"]
                .as_u64()
                .map(|v| v as u32);
            let output = resp_json["usage"]["completion_tokens"]
                .as_u64()
                .map(|v| v as u32);
            let cache_read = resp_json["usage"]["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .map(|v| v as u32);
            match (input, output) {
                (Some(i), Some(o)) => Some(crate::llm::types::TokenUsage {
                    input_tokens: i,
                    output_tokens: o,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: cache_read,
                }),
                _ => None,
            }
        };
        Ok(LlmResponse {
            message,
            stop_reason,
            usage,
        })
    }

    fn provider_name(&self) -> &str {
        "openai"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    fn context_window(&self) -> u32 {
        self.context_window_inner()
    }
}

#[async_trait]
impl ReactLLM for ChatOpenAI {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
    ) -> AgentResult<Reasoning> {
        let tool_defs = tools.iter().map(|t| t.definition()).collect();
        let request = LlmRequest::new(messages.to_vec()).with_tools(tool_defs);

        let response = self.invoke(request).await?;
        let usage = response.usage.clone();
        let model_name = self.model.clone();

        if response.stop_reason == StopReason::ToolUse {
            let blocks = response.message.content_blocks();
            let thought = blocks
                .iter()
                .filter_map(|b| b.as_text())
                .collect::<Vec<_>>()
                .join("");

            let calls: Vec<ToolCall> = blocks
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::ToolUse { id, name, input } = b {
                        Some(ToolCall::new(id.clone(), name.clone(), input.clone()))
                    } else {
                        None
                    }
                })
                .collect();

            if !calls.is_empty() {
                let mut r = Reasoning::with_tools(thought, calls);
                r.source_message = Some(response.message);
                r.usage = usage;
                r.model = model_name;
                return Ok(r);
            }

            let calls: Vec<ToolCall> = response
                .message
                .tool_calls()
                .iter()
                .map(|tc| ToolCall::new(tc.id.clone(), tc.name.clone(), tc.arguments.clone()))
                .collect();
            let mut r = Reasoning::with_tools(thought, calls);
            r.source_message = Some(response.message);
            r.usage = usage;
            r.model = model_name;
            Ok(r)
        } else {
            let text = response.message.content();
            let mut r = Reasoning::with_answer("", text);
            r.source_message = Some(response.message);
            r.usage = usage;
            r.model = model_name;
            Ok(r)
        }
    }

    fn model_name(&self) -> String {
        self.model.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reasoning block 默认被过滤（大多数 provider 不支持 thinking content type）
    #[test]
    fn test_reasoning_block_filtered_by_default() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::reasoning("step 1"),
            ContentBlock::text("answer"),
        ]);
        let val = ChatOpenAI::content_to_openai(&content, false);
        let arr = val.as_array().expect("content 应为 array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "answer");
    }

    /// supports_thinking_content=true 时 Reasoning block 应序列化为 thinking 类型
    #[test]
    fn test_reasoning_block_included_when_supported() {
        let content = MessageContent::Blocks(vec![
            ContentBlock::reasoning("step 1"),
            ContentBlock::text("answer"),
        ]);
        let val = ChatOpenAI::content_to_openai(&content, true);
        let arr = val.as_array().expect("content 应为 array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "thinking");
        assert_eq!(arr[0]["thinking"], "step 1");
        assert_eq!(arr[1]["type"], "text");
        assert_eq!(arr[1]["text"], "answer");
    }

    /// 仅 reasoning block 无 text 时，content 应为空字符串
    #[test]
    fn test_reasoning_only_block_becomes_empty() {
        let content = MessageContent::Blocks(vec![ContentBlock::reasoning("deep thinking")]);
        let val = ChatOpenAI::content_to_openai(&content, false);
        assert_eq!(val, json!(""));
    }

    /// messages_to_json：默认模型不支持 thinking，reasoning 从 content 过滤但回传到 reasoning_content 顶层字段
    #[test]
    fn test_messages_to_json_with_reasoning_filtered() {
        let llm = ChatOpenAI::new("sk-test", "gpt-4o");
        assert!(!llm.supports_thinking_content);
        let msgs = vec![BaseMessage::ai_from_blocks(vec![
            ContentBlock::reasoning("r1"),
            ContentBlock::text("t1"),
        ])];
        let vals = llm.messages_to_json(&msgs);
        assert_eq!(vals.len(), 1);
        let assistant = &vals[0];
        assert_eq!(assistant["role"], "assistant");
        // content 中 reasoning 被过滤，只剩 text
        let content = assistant["content"].as_array().expect("content 应为 array");
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "t1");
        // reasoning_content 顶层字段回传
        assert_eq!(assistant["reasoning_content"], "r1");
    }

    /// messages_to_json：deepseek-v4-pro 支持 thinking，content 中保留且同时回传 reasoning_content
    #[test]
    fn test_messages_to_json_with_reasoning_included_for_deepseek_v4() {
        let llm = ChatOpenAI::new("sk-test", "deepseek-v4-pro");
        assert!(llm.supports_thinking_content);
        let msgs = vec![BaseMessage::ai_from_blocks(vec![
            ContentBlock::reasoning("r1"),
            ContentBlock::text("t1"),
        ])];
        let vals = llm.messages_to_json(&msgs);
        assert_eq!(vals.len(), 1);
        let assistant = &vals[0];
        let content = assistant["content"].as_array().expect("content 应为 array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "r1");
        assert_eq!(content[1]["type"], "text");
        // reasoning_content 顶层字段也回传
        assert_eq!(assistant["reasoning_content"], "r1");
    }

    /// messages_to_json：DeepSeek R1 reasoning_content 回传 + tool_calls
    #[test]
    fn test_messages_to_json_reasoning_with_tool_calls() {
        let llm = ChatOpenAI::new("sk-test", "deepseek-r1");
        let msgs = vec![BaseMessage::ai_from_blocks(vec![
            ContentBlock::reasoning("need bash"),
            ContentBlock::text("running..."),
            ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
        ])];
        let vals = llm.messages_to_json(&msgs);
        let assistant = &vals[0];
        // reasoning_content 顶层字段
        assert_eq!(assistant["reasoning_content"], "need bash");
        // tool_calls 在顶层
        assert!(assistant["tool_calls"].is_array());
        assert_eq!(assistant["tool_calls"][0]["id"], "tc1");
    }

    /// 无 reasoning 的纯文本 AI 消息，content 应为字符串（保持兼容）
    #[test]
    fn test_messages_to_json_text_only() {
        let llm = ChatOpenAI::new("sk-test", "gpt-4o");
        let msgs = vec![BaseMessage::ai("hello")];
        let vals = llm.messages_to_json(&msgs);
        let assistant = &vals[0];
        assert_eq!(assistant["role"], "assistant");
        assert!(assistant["content"].is_string());
        assert_eq!(assistant["content"], "hello");
    }

    /// 格式错误的 JSON 工具参数应被记录并保留原始内容而非静默丢弃
    #[test]
    fn test_malformed_tool_args_preserved() {
        let args_str = "{invalid json";
        let arguments = match serde_json::from_str::<Value>(args_str) {
            Ok(v) => v,
            Err(_) => serde_json::json!({"_raw_arguments": args_str}),
        };
        assert!(
            arguments.get("_raw_arguments").is_some(),
            "格式错误的参数应保留在 _raw_arguments 中: {arguments}"
        );
    }

    /// context_window: gpt-4 系列应返回 128K
    #[test]
    fn test_context_window_gpt4() {
        let llm = ChatOpenAI::new("sk-test", "gpt-4o");
        assert_eq!(llm.context_window_inner(), 128_000);
    }

    /// context_window: gpt-3.5-turbo 应返回 16K
    #[test]
    fn test_context_window_gpt35() {
        let llm = ChatOpenAI::new("sk-test", "gpt-3.5-turbo");
        assert_eq!(llm.context_window_inner(), 16_385);
    }

    /// context_window: o1 系列应返回 200K
    #[test]
    fn test_context_window_o1() {
        let llm = ChatOpenAI::new("sk-test", "o1-preview");
        assert_eq!(llm.context_window_inner(), 200_000);
    }

    /// context_window: deepseek 系列应返回 128K
    #[test]
    fn test_context_window_deepseek() {
        let llm = ChatOpenAI::new("sk-test", "deepseek-r1");
        assert_eq!(llm.context_window_inner(), 128_000);
    }

    /// context_window: 未知模型回退默认 200K
    #[test]
    fn test_context_window_unknown() {
        let llm = ChatOpenAI::new("sk-test", "custom-model");
        assert_eq!(llm.context_window_inner(), 200_000);
    }

    // ── Builder method tests ──

    #[test]
    fn test_with_base_url() {
        let llm = ChatOpenAI::new("key", "model").with_base_url("https://proxy.example.com/v1");
        assert_eq!(llm.base_url, "https://proxy.example.com/v1");
    }

    #[test]
    fn test_with_reasoning_effort() {
        let llm = ChatOpenAI::new("key", "o1-preview").with_reasoning_effort("high");
        assert_eq!(llm.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn test_with_thinking_content() {
        let llm = ChatOpenAI::new("key", "gpt-4o").with_thinking_content(true);
        assert!(llm.supports_thinking_content);
    }

    #[test]
    fn test_with_thinking_enabled() {
        let llm = ChatOpenAI::new("key", "deepseek-v4-pro").with_thinking_enabled();
        assert!(llm.thinking_enabled, "thinking_enabled 应为 true");
        // deepseek-v4-pro 的 supports_thinking_content 由构造函数自动检测开启
        assert!(
            llm.supports_thinking_content,
            "deepseek-v4-pro 应自动检测 supports_thinking_content"
        );
    }

    #[test]
    fn test_with_thinking_enabled_non_v4() {
        // 非 v4 模型：thinking_enabled 开启但 supports_thinking_content 保持 false
        let llm = ChatOpenAI::new("key", "deepseek-chat").with_thinking_enabled();
        assert!(llm.thinking_enabled);
        assert!(
            !llm.supports_thinking_content,
            "非 v4 模型不应开启 supports_thinking_content，否则 content 数组中会发送不支持的 thinking 块"
        );
    }

    #[test]
    fn test_detect_thinking_content_deepseek_v4() {
        assert!(ChatOpenAI::detect_thinking_content_support(
            "deepseek-v4-pro"
        ));
        assert!(ChatOpenAI::detect_thinking_content_support(
            "DeepSeek-V4-Pro"
        ));
        assert!(!ChatOpenAI::detect_thinking_content_support("deepseek-r1"));
        assert!(!ChatOpenAI::detect_thinking_content_support("gpt-4o"));
    }

    #[test]
    fn test_new_default_no_reasoning_effort() {
        let llm = ChatOpenAI::new("key", "gpt-4o");
        assert!(llm.reasoning_effort.is_none());
        assert_eq!(llm.base_url, "https://api.openai.com/v1");
    }

    /// context_window: o3 系列应返回 200K
    #[test]
    fn test_context_window_o3() {
        let llm = ChatOpenAI::new("sk-test", "o3-mini");
        assert_eq!(llm.context_window_inner(), 200_000);
    }

    /// 验证多轮 tool call 对话的消息序列：每个 tool 消息前面必须是 assistant with tool_calls
    #[test]
    fn test_messages_to_json_tool_sequence_valid() {
        let llm = ChatOpenAI::new("sk-test", "deepseek-r1");
        let msgs = vec![
            BaseMessage::system("You are helpful"),
            BaseMessage::human("list files"),
            // 第一轮 tool call
            BaseMessage::ai_from_blocks(vec![
                ContentBlock::reasoning("need ls"),
                ContentBlock::text("running ls"),
                ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
            ]),
            BaseMessage::tool_result("tc1", "file1.rs\nfile2.rs"),
            // 第二轮 tool call
            BaseMessage::ai_from_blocks(vec![
                ContentBlock::reasoning("read file"),
                ContentBlock::text("reading"),
                ContentBlock::tool_use("tc2", "Read", json!({"path": "file1.rs"})),
            ]),
            BaseMessage::tool_result("tc2", "fn main() {}"),
            // 最终回答
            BaseMessage::ai_from_blocks(vec![
                ContentBlock::reasoning("done"),
                ContentBlock::text("Here is the file content"),
            ]),
        ];

        let vals = llm.messages_to_json(&msgs);

        // 验证：每个 tool 消息前面的消息必须有 tool_calls
        for (i, msg) in vals.iter().enumerate() {
            if msg["role"] == "tool" {
                assert!(i > 0, "tool 消息不能是第一条: {:?}", msg);
                let prev = &vals[i - 1];
                assert!(
                    prev["role"] == "assistant" && prev["tool_calls"].is_array(),
                    "tool 消息前必须是 assistant with tool_calls，实际前一条: {:?}",
                    prev
                );
            }
        }

        // 验证 system 在最前
        assert_eq!(vals[0]["role"], "system");
    }

    /// 验证 micro compact 后的消息序列仍然合法
    #[test]
    fn test_messages_to_json_after_micro_compact() {
        let llm = ChatOpenAI::new("sk-test", "deepseek-r1");
        // micro compact 后：tool 结果被替换为 "[compacted: ...]"，但消息不删除
        let msgs = vec![
            BaseMessage::system("system"),
            BaseMessage::human("list"),
            BaseMessage::ai_from_blocks(vec![
                ContentBlock::reasoning("need bash"),
                ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
            ]),
            BaseMessage::tool_result("tc1", "[compacted: 1000 chars]"),
            BaseMessage::ai_from_blocks(vec![
                ContentBlock::reasoning("now read"),
                ContentBlock::tool_use("tc2", "Read", json!({"path": "f.rs"})),
            ]),
            BaseMessage::tool_result("tc2", "[compacted: 500 chars]"),
            BaseMessage::ai("done"),
        ];

        let vals = llm.messages_to_json(&msgs);
        for (i, msg) in vals.iter().enumerate() {
            if msg["role"] == "tool" {
                let prev = &vals[i - 1];
                assert!(
                    prev["role"] == "assistant" && prev["tool_calls"].is_array(),
                    "micro compact 后 tool 序列非法，位置 {}: 前一条 {:?}",
                    i,
                    prev
                );
            }
        }
    }

    /// 端到端验证 deepseek-v4-pro：模拟 API 响应 → parse_assistant_message → 序列化回传
    ///
    /// 验证 thinking 内容在完整链路中不丢失。
    #[test]
    fn test_deepseek_v4_pro_thinking_roundtrip() {
        // 模拟 deepseek-v4-pro API 响应（含 reasoning_content + tool_calls）
        let api_response = json!({
            "role": "assistant",
            "content": "Let me check the weather for you.",
            "reasoning_content": "I need to first get the current date, then check the weather.",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "get_date",
                    "arguments": "{}"
                }
            }]
        });

        let message = ChatOpenAI::parse_assistant_message(&api_response, &StopReason::ToolUse);

        // 验证解析：message 应包含 Reasoning + Text + ToolUse blocks
        assert!(message.has_tool_calls());
        let blocks = message.content_blocks();
        assert_eq!(
            blocks.len(),
            3,
            "应有 Reasoning + Text + ToolUse 三个 blocks"
        );

        match &blocks[0] {
            ContentBlock::Reasoning { text, .. } => {
                assert_eq!(
                    text,
                    "I need to first get the current date, then check the weather."
                );
            }
            other => panic!("第一个 block 应为 Reasoning，实际为 {:?}", other),
        }
        assert_eq!(
            blocks[1].as_text(),
            Some("Let me check the weather for you.")
        );
        assert!(matches!(&blocks[2], ContentBlock::ToolUse { .. }));

        // 模拟第二轮序列化（deepseek-v4-pro with thinking_enabled）
        let llm = ChatOpenAI::new("sk-test", "deepseek-v4-pro").with_thinking_enabled();
        let tool_result = BaseMessage::tool_result("call_1", "2025-05-11");
        let messages = vec![
            BaseMessage::human("How's the weather tomorrow?"),
            message,
            tool_result,
        ];

        let vals = llm.messages_to_json(&messages);
        let assistant = vals.iter().find(|m| m["role"] == "assistant").unwrap();

        // 验证 reasoning_content 顶层字段回传（deepseek-v4-pro 要求）
        assert_eq!(
            assistant["reasoning_content"],
            "I need to first get the current date, then check the weather.",
            "reasoning_content 顶层字段必须回传，否则 deepseek 返回 400"
        );

        // 验证 content 中也包含 thinking block（deepseek-v4-pro supports_thinking_content=true）
        let content = assistant["content"].as_array().expect("content 应为数组");
        let thinking_block = content.iter().find(|b| b["type"] == "thinking");
        assert!(
            thinking_block.is_some(),
            "deepseek-v4-pro content 中应包含 thinking block"
        );
        assert_eq!(
            thinking_block.unwrap()["thinking"],
            "I need to first get the current date, then check the weather."
        );

        // 验证 tool_calls 正确序列化
        assert!(assistant["tool_calls"].is_array());
        assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "get_date");
    }

    /// 验证 deepseek-v4-pro 多轮对话中每轮的 thinking 都被回传
    #[test]
    fn test_deepseek_v4_pro_multi_turn_thinking() {
        let llm = ChatOpenAI::new("sk-test", "deepseek-v4-pro").with_thinking_enabled();

        let msgs = vec![
            BaseMessage::human("question 1"),
            // 第一轮 assistant
            BaseMessage::ai_from_blocks(vec![
                ContentBlock::reasoning("thinking round 1"),
                ContentBlock::text("answer 1"),
                ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
            ]),
            BaseMessage::tool_result("tc1", "result 1"),
            // 第二轮 assistant
            BaseMessage::ai_from_blocks(vec![
                ContentBlock::reasoning("thinking round 2"),
                ContentBlock::text("answer 2"),
                ContentBlock::tool_use("tc2", "Read", json!({"path": "f.txt"})),
            ]),
            BaseMessage::tool_result("tc2", "result 2"),
            BaseMessage::human("question 2"),
        ];

        let vals = llm.messages_to_json(&msgs);
        let assistant_msgs: Vec<_> = vals.iter().filter(|m| m["role"] == "assistant").collect();

        assert_eq!(assistant_msgs.len(), 2, "应有 2 条 assistant 消息");

        // 第一轮
        assert_eq!(assistant_msgs[0]["reasoning_content"], "thinking round 1");
        assert_eq!(assistant_msgs[0]["tool_calls"][0]["id"], "tc1");

        // 第二轮
        assert_eq!(assistant_msgs[1]["reasoning_content"], "thinking round 2");
        assert_eq!(assistant_msgs[1]["tool_calls"][0]["id"], "tc2");
    }

    /// deepseek-v4-pro 返回 content 数组格式（thinking 块在 content 内，无顶层 reasoning_content）
    ///
    /// 某些 API 实现将 thinking 内容放在 content 数组中而非 reasoning_content 顶层字段。
    /// 解析器必须从 content 数组中提取 thinking 块。
    #[test]
    fn test_deepseek_v4_pro_content_array_thinking() {
        let api_response = json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "Let me analyze this..."},
                {"type": "text", "text": "I'll run a command."}
            ],
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Bash", "arguments": "{\"command\":\"ls\"}"}
            }]
        });

        let message = ChatOpenAI::parse_assistant_message(&api_response, &StopReason::ToolUse);
        let blocks = message.content_blocks();
        assert!(message.has_tool_calls());

        // 应包含 Reasoning（从 content 数组提取）+ Text + ToolUse
        assert!(blocks.iter().any(|b| matches!(b, ContentBlock::Reasoning { text, .. } if text == "Let me analyze this...")),
            "应从 content 数组中提取 thinking 块为 Reasoning block");
        assert!(blocks
            .iter()
            .any(|b| b.as_text() == Some("I'll run a command.")));

        // 序列化回传时应包含 reasoning_content 顶层字段
        let llm = ChatOpenAI::new("sk-test", "deepseek-v4-pro").with_thinking_enabled();
        let vals = llm.messages_to_json(&[message]);
        let assistant = &vals[0];
        assert_eq!(
            assistant["reasoning_content"], "Let me analyze this...",
            "从 content 数组提取的 thinking 必须作为 reasoning_content 回传"
        );
    }

    /// deepseek-v4-pro 同时返回顶层 reasoning_content 和 content 数组中的 thinking 块
    ///
    /// 应优先使用顶层 reasoning_content，跳过 content 数组中的重复 thinking 块。
    #[test]
    fn test_deepseek_v4_pro_both_reasoning_sources() {
        let api_response = json!({
            "role": "assistant",
            "reasoning_content": "top-level reasoning",
            "content": [
                {"type": "thinking", "thinking": "duplicate in array"},
                {"type": "text", "text": "answer"}
            ],
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Bash", "arguments": "{}"}
            }]
        });

        let message = ChatOpenAI::parse_assistant_message(&api_response, &StopReason::ToolUse);
        let blocks = message.content_blocks();

        // 只应有一个 Reasoning block（来自顶层），不应重复
        let reasoning_count = blocks
            .iter()
            .filter(|b| matches!(b, ContentBlock::Reasoning { .. }))
            .count();
        assert_eq!(reasoning_count, 1, "不应重复 Reasoning block");
        assert_eq!(
            blocks.iter().find_map(|b| b.as_reasoning()),
            Some("top-level reasoning"),
            "应优先使用顶层 reasoning_content"
        );
    }

    /// content 为空数组时的退化场景
    #[test]
    fn test_deepseek_v4_pro_empty_content_array() {
        let api_response = json!({
            "role": "assistant",
            "content": [],
            "reasoning_content": "thinking...",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "Bash", "arguments": "{}"}
            }]
        });

        let message = ChatOpenAI::parse_assistant_message(&api_response, &StopReason::ToolUse);
        assert!(message.has_tool_calls());
        let blocks = message.content_blocks();
        assert!(blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Reasoning { .. })));
    }

    /// DeepSeek v4 thinking 模式：assistant 消息没有 Reasoning block 时自动注入空 reasoning_content
    ///
    /// LLM 有时返回空的 reasoning_content（被 parse 跳过），但 API 要求必须回传该字段。
    #[test]
    fn test_deepseek_v4_empty_reasoning_auto_inject() {
        let llm = ChatOpenAI::new("sk-test", "deepseek-v4-flash").with_thinking_enabled();

        // 模拟 assistant 消息没有 Reasoning block（reasoning_content 为空被跳过）
        let msgs = vec![
            BaseMessage::human("question"),
            BaseMessage::ai_from_blocks(vec![
                ContentBlock::text("I'll run a command."),
                ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
            ]),
            BaseMessage::tool_result("tc1", "result"),
        ];

        let vals = llm.messages_to_json(&msgs);
        let assistant = vals.iter().find(|m| m["role"] == "assistant").unwrap();

        // 验证 reasoning_content 字段存在（即使为空字符串）
        assert!(
            assistant.get("reasoning_content").is_some(),
            "thinking 模式下 assistant 消息必须包含 reasoning_content 字段"
        );
        assert_eq!(
            assistant["reasoning_content"].as_str().unwrap(),
            "",
            "无 reasoning 内容时应注入空字符串"
        );
    }

    /// 非 thinking 模式：不注入 reasoning_content
    #[test]
    fn test_non_thinking_no_reasoning_inject() {
        let llm = ChatOpenAI::new("sk-test", "gpt-4o");

        let msgs = vec![
            BaseMessage::human("question"),
            BaseMessage::ai_from_blocks(vec![
                ContentBlock::text("answer"),
                ContentBlock::tool_use("tc1", "Bash", json!({"command": "ls"})),
            ]),
            BaseMessage::tool_result("tc1", "result"),
        ];

        let vals = llm.messages_to_json(&msgs);
        let assistant = vals.iter().find(|m| m["role"] == "assistant").unwrap();

        // 非 thinking 模式不应注入 reasoning_content
        assert!(
            assistant.get("reasoning_content").is_none(),
            "非 thinking 模式不应注入 reasoning_content"
        );
    }
}
