use crate::messages::BaseMessage;
use crate::tools::ToolDefinition;

/// LLM 请求
pub struct LlmRequest {
    pub messages: Vec<BaseMessage>,
    pub tools: Vec<ToolDefinition>,
    /// Anthropic system 字段（OpenAI 通过 System 消息传递）
    pub system: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// 会话级 ID，用于 LiteLLM 等代理按 session 聚合多次 LLM 请求
    pub session_id: Option<String>,
}

impl LlmRequest {
    pub fn new(messages: Vec<BaseMessage>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            system: None,
            max_tokens: None,
            temperature: None,
            session_id: None,
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

/// Token 使用量（adapter 层规范化后的统一语义，所有 provider 一致）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TokenUsage {
    /// 总输入 token（含缓存 token，adapter 层已规范化）
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// 写入缓存的 token 数（仅 Anthropic 有意义，OpenAI 始终 None）
    pub cache_creation_input_tokens: Option<u32>,
    /// 从缓存读取的 token 数（Anthropic/OpenAI 均有，某些模型为 None）
    pub cache_read_input_tokens: Option<u32>,
    /// API 提供商返回的请求 ID
    pub request_id: Option<String>,
}

/// LLM 响应
pub struct LlmResponse {
    /// Ai 变体消息
    pub message: BaseMessage,
    pub stop_reason: StopReason,
    /// Token 使用量（可选，不支持的 LLM 为 None）
    pub usage: Option<TokenUsage>,
    /// API 提供商返回的请求 ID
    pub request_id: Option<String>,
}

/// 停止原因
#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other(String),
}

impl StopReason {
    pub fn from_openai(s: &str) -> Self {
        match s {
            "stop" => Self::EndTurn,
            "tool_calls" => Self::ToolUse,
            "length" => Self::MaxTokens,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn from_anthropic(s: &str) -> Self {
        match s {
            "end_turn" => Self::EndTurn,
            "tool_use" => Self::ToolUse,
            "max_tokens" => Self::MaxTokens,
            other => Self::Other(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_openai_stop() {
        assert_eq!(StopReason::from_openai("stop"), StopReason::EndTurn);
    }

    #[test]
    fn test_from_openai_tool_calls() {
        assert_eq!(StopReason::from_openai("tool_calls"), StopReason::ToolUse);
    }

    #[test]
    fn test_from_openai_length() {
        assert_eq!(StopReason::from_openai("length"), StopReason::MaxTokens);
    }

    #[test]
    fn test_from_openai_unknown() {
        assert!(matches!(
            StopReason::from_openai("content_filter"),
            StopReason::Other(_)
        ));
    }

    #[test]
    fn test_from_anthropic_end_turn() {
        assert_eq!(StopReason::from_anthropic("end_turn"), StopReason::EndTurn);
    }

    #[test]
    fn test_from_anthropic_tool_use() {
        assert_eq!(StopReason::from_anthropic("tool_use"), StopReason::ToolUse);
    }

    #[test]
    fn test_from_anthropic_max_tokens() {
        assert_eq!(
            StopReason::from_anthropic("max_tokens"),
            StopReason::MaxTokens
        );
    }

    #[test]
    fn test_from_anthropic_unknown() {
        assert!(matches!(
            StopReason::from_anthropic("pause_turn"),
            StopReason::Other(_)
        ));
    }

    #[test]
    fn test_stop_reason_equality() {
        assert_eq!(StopReason::EndTurn, StopReason::EndTurn);
        assert_ne!(StopReason::EndTurn, StopReason::ToolUse);
    }
}
