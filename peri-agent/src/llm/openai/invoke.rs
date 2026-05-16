use async_trait::async_trait;
use serde_json::{json, Value};

use super::super::BaseModel;
use super::ChatOpenAI;
use crate::agent::react::{ReactLLM, Reasoning, ToolCall};
use crate::error::{AgentError, AgentResult};
use crate::llm::types::{LlmRequest, LlmResponse, StopReason, StreamingContext};
use crate::messages::{BaseMessage, ContentBlock, ImageSource, MessageContent, ToolCallRequest};
use crate::tools::BaseTool;

fn block_to_openai_part(block: &ContentBlock, supports_thinking_content: bool) -> Option<Value> {
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
        // Document / Unknown 透传为 raw JSON
        ContentBlock::Document { source, title } => {
            let src = serde_json::to_value(source).unwrap_or_default();
            Some(json!({ "type": "document", "source": src, "title": title }))
        }
        ContentBlock::Unknown(v) => Some(v.clone()),
    }
}

pub(super) fn content_to_openai(
    content: &MessageContent,
    supports_thinking_content: bool,
) -> Value {
    match content {
        MessageContent::Text(s) => json!(s),
        MessageContent::Blocks(blocks) => {
            let parts: Vec<Value> = blocks
                .iter()
                .filter_map(|b| block_to_openai_part(b, supports_thinking_content))
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

pub(super) fn messages_to_json(adapter: &ChatOpenAI, messages: &[BaseMessage]) -> Vec<Value> {
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
                result.push(json!({
                    "role": "user",
                    "content": content_to_openai(content, adapter.supports_thinking_content)
                }));
            }
            BaseMessage::Ai {
                content,
                tool_calls,
                ..
            } => {
                // 提取 reasoning 文本（DeepSeek R1 要求回传 reasoning_content 顶层字段）
                let reasoning_text = extract_reasoning_text(content);
                let serialized_content =
                    content_to_openai(content, adapter.supports_thinking_content);

                // 所有 assistant 消息都包含 reasoning_content 字段，确保 thinking 内容跨轮次不丢失
                // 同时设置 reasoning 字段（GLM 系列模型使用此字段名）
                if tool_calls.is_empty() {
                    let mut msg = json!({ "role": "assistant", "content": serialized_content });
                    let reasoning_val = json!(reasoning_text.as_deref().unwrap_or(""));
                    msg["reasoning_content"] = reasoning_val.clone();
                    msg["reasoning"] = reasoning_val;
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
                    let reasoning_val = json!(reasoning_text.as_deref().unwrap_or(""));
                    msg["reasoning_content"] = reasoning_val.clone();
                    msg["reasoning"] = reasoning_val;
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
                    "content": content_to_openai(content, adapter.supports_thinking_content)
                }));
            }
        }
    }

    if !system_parts.is_empty() {
        let system_text = system_parts
            .join("\n\n")
            .replace("__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__", "");
        result.insert(0, json!({ "role": "system", "content": system_text }));
    }

    result
}

pub(super) fn parse_assistant_message(
    assistant_msg: &Value,
    stop_reason: &StopReason,
) -> BaseMessage {
    // 检测 content 是字符串还是数组
    let content_val = &assistant_msg["content"];
    let is_array = content_val.is_array();

    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();

    // 1) reasoning_content 顶层字段（deepseek-r1、某些 OpenAI o 系列）
    //    也检查 reasoning 字段（GLM 系列模型使用此字段名）
    let mut has_top_level_reasoning = false;
    let reasoning_text = assistant_msg["reasoning_content"]
        .as_str()
        .or_else(|| assistant_msg["reasoning"].as_str());
    if let Some(reasoning) = reasoning_text {
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

/// 校验消息序列不变量：每段连续 tool 消息块之前必须有 assistant with tool_calls
pub(super) fn validate_message_invariants(messages: &[Value]) {
    let mut i = 0;
    while i < messages.len() {
        if messages[i]["role"] == "tool" {
            let block_start = i;
            let prev_non_tool = if block_start > 0 {
                let mut j = block_start;
                while j > 0 && messages[j - 1]["role"] == "tool" {
                    j -= 1;
                }
                if j > 0 {
                    Some(&messages[j - 1])
                } else {
                    None
                }
            } else {
                None
            };
            let valid = prev_non_tool
                .is_some_and(|p| p["role"] == "assistant" && p["tool_calls"].is_array());
            if !valid {
                tracing::error!(
                    block_start,
                    total = messages.len(),
                    prev_non_tool_role = ?prev_non_tool.map(|m| m["role"].as_str()),
                    "消息序列不变量违反：连续 tool 块前缺少 assistant with tool_calls"
                );
            }
            // 跳过整个 tool 块
            while i < messages.len() && messages[i]["role"] == "tool" {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
}

/// 构建 OpenAI API 请求体（invoke 和 invoke_streaming 共用）
pub(super) fn build_request_body(
    adapter: &ChatOpenAI,
    request: &LlmRequest,
    streaming: bool,
) -> Value {
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

    let mut messages = messages_to_json(adapter, &request.messages);

    validate_message_invariants(&messages);

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
        "model": adapter.model,
        "messages": messages,
        "stream": streaming
    });

    if streaming {
        body["stream_options"] = json!({"include_usage": true});
    }

    if !tools_json.is_empty() {
        body["tools"] = Value::Array(tools_json);
        body["tool_choice"] = json!("auto");
    }

    let max_tokens = request.max_tokens.unwrap_or(adapter.max_tokens);
    body["max_tokens"] = json!(max_tokens);

    if let Some(ref effort) = adapter.reasoning_effort {
        // o1/o3 reasoning effort 模式：加 reasoning_effort，不设 temperature
        body["reasoning_effort"] = json!(effort);
    } else if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }

    // DeepSeek thinking 模式（deepseek-v4-pro 等）
    if adapter.thinking_enabled {
        body["thinking"] = json!({ "type": "enabled" });
    }

    // LiteLLM session tracking：通过 metadata.session_id 按 session 聚合多次请求
    if let Some(ref sid) = request.session_id {
        body["metadata"] = json!({ "session_id": sid });
    }

    body
}

#[async_trait]
impl BaseModel for ChatOpenAI {
    async fn invoke(&self, request: LlmRequest) -> AgentResult<LlmResponse> {
        let msg_count = request.messages.len();
        let start = std::time::Instant::now();

        let body = build_request_body(self, &request, false);

        let chat_url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

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

        let message = parse_assistant_message(assistant_msg, &stop_reason);

        let request_id = resp_json["id"].as_str().map(|s| s.to_string());

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
                    request_id: request_id.clone(),
                }),
                _ => None,
            }
        };
        Ok(LlmResponse {
            message,
            stop_reason,
            usage,
            request_id,
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

    async fn invoke_streaming(
        &self,
        request: LlmRequest,
        ctx: StreamingContext,
    ) -> AgentResult<LlmResponse> {
        super::stream::do_invoke_streaming(self, request, ctx).await
    }
}

#[async_trait]
impl ReactLLM for ChatOpenAI {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
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
            // 防御：某些 provider 可能返回 stop_reason != ToolUse 但响应含 tool_use blocks
            let blocks = response.message.content_blocks();
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
                tracing::warn!(
                    stop_reason = ?response.stop_reason,
                    tool_count = calls.len(),
                    "stop_reason 与内容不一致：响应含 tool_use blocks 但 stop_reason 非 ToolUse，按工具调用处理"
                );
                let thought = blocks
                    .iter()
                    .filter_map(|b| b.as_text())
                    .collect::<Vec<_>>()
                    .join("");
                let mut r = Reasoning::with_tools(thought, calls);
                r.source_message = Some(response.message);
                r.usage = usage;
                r.model = model_name;
                return Ok(r);
            }

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
