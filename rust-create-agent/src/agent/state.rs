use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::messages::BaseMessage;
use crate::thread::ThreadId;
use crate::thread::ThreadStore;

/// State trait - 所有 Agent 状态必须实现此 trait
/// 与 TypeScript BaseAgentStateType 对齐
pub trait State: Send + Sync + Clone + 'static {
    fn cwd(&self) -> &str;
    fn set_cwd(&mut self, cwd: impl Into<String>);
    fn messages(&self) -> &[BaseMessage];
    fn add_message(&mut self, message: BaseMessage);

    /// 将消息前插到消息历史开头（系统消息置于最前）
    fn prepend_message(&mut self, message: BaseMessage);

    fn current_step(&self) -> usize;
    fn set_current_step(&mut self, step: usize);

    fn get_context(&self, key: &str) -> Option<&str>;
    fn set_context(&mut self, key: impl Into<String>, value: impl Into<String>);

    fn token_tracker(&self) -> &crate::agent::token::TokenTracker;
    fn token_tracker_mut(&mut self) -> &mut crate::agent::token::TokenTracker;

    /// 获取消息的可变引用（用于 micro_compact 等原地修改场景）
    fn messages_mut(&mut self) -> &mut Vec<BaseMessage>;
}

/// 基础 Agent 状态（与 TypeScript BaseAgentStateType 对齐）
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct AgentState {
    pub cwd: String,
    #[serde(skip)]
    pub messages: Vec<BaseMessage>,
    pub current_step: usize,
    pub context: HashMap<String, String>,
    pub token_tracker: crate::agent::token::TokenTracker,
    /// 可选持久化后端（绑定后 add_message 自动写入）
    #[serde(skip)]
    store: Option<Arc<dyn ThreadStore>>,
    /// 持久化目标 thread id
    #[serde(skip)]
    thread_id: Option<ThreadId>,
    /// 有序持久化通道：保证消息按 add_message 调用顺序写入 SQLite，
    /// 避免 tokio::spawn 的 fire-and-forget 模式因 .await 让步导致乱序。
    #[serde(skip)]
    persist_tx: Option<Arc<tokio::sync::mpsc::UnboundedSender<BaseMessage>>>,
}

impl std::fmt::Debug for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentState")
            .field("cwd", &self.cwd)
            .field("messages", &self.messages)
            .field("current_step", &self.current_step)
            .field("context", &self.context)
            .field("store", &self.store.as_ref().map(|_| "ThreadStore"))
            .field("thread_id", &self.thread_id)
            .field("token_tracker", &self.token_tracker)
            .finish()
    }
}

impl AgentState {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            ..Default::default()
        }
    }

    /// 从已有消息历史构建（用于多轮对话续接）
    pub fn with_messages(cwd: impl Into<String>, messages: Vec<BaseMessage>) -> Self {
        Self {
            cwd: cwd.into(),
            messages,
            ..Default::default()
        }
    }

    /// 消费 state，返回消息历史（用于传回调用方保存）
    pub fn into_messages(self) -> Vec<BaseMessage> {
        self.messages
    }

    /// 绑定持久化后端，之后每次 add_message 自动写入
    ///
    /// 使用有序通道 + 专用 writer 任务替代 fire-and-forget tokio::spawn，
    /// 保证消息按 add_message 调用顺序写入 SQLite，
    /// 避免 spawn 任务的 .await 让步导致 rowid 乱序（#history-restore-bug）。
    pub fn with_persistence(
        mut self,
        store: Arc<dyn ThreadStore>,
        thread_id: impl Into<String>,
    ) -> Self {
        self.store = Some(store.clone());
        self.thread_id = Some(thread_id.into());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BaseMessage>();
        self.persist_tx = Some(Arc::new(tx));
        let tid = self.thread_id.clone().unwrap();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = store.append_message(&tid, msg).await {
                    tracing::warn!("ordered persist failed: {e}");
                }
            }
        });
        self
    }

    pub fn with_context(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.context.insert(key.into(), value.into());
        self
    }

    pub fn get_context(&self, key: &str) -> Option<&str> {
        self.context.get(key).map(|s| s.as_str())
    }

    pub fn set_context(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.context.insert(key.into(), value.into());
    }
}

impl State for AgentState {
    fn cwd(&self) -> &str {
        &self.cwd
    }

    fn set_cwd(&mut self, cwd: impl Into<String>) {
        self.cwd = cwd.into();
    }

    fn messages(&self) -> &[BaseMessage] {
        &self.messages
    }

    fn add_message(&mut self, message: BaseMessage) {
        // 有序持久化：通过通道发送到专用 writer 任务，保证写入顺序
        if let Some(ref tx) = self.persist_tx {
            if let Err(e) = tx.send(message.clone()) {
                tracing::warn!("ordered persist send failed (channel closed): {e}");
            }
        }
        self.messages.push(message);
        // 消息数量超过阈值时发出警告，提示使用 /compact 压缩上下文以降低内存占用
        let count = self.messages.len();
        if count > 100 && count.is_multiple_of(100) {
            tracing::warn!(
                count,
                "AgentState: message history is large ({} messages); consider using /compact to reduce memory usage",
                count
            );
        }
    }

    fn prepend_message(&mut self, message: BaseMessage) {
        self.messages.insert(0, message);
    }

    fn current_step(&self) -> usize {
        self.current_step
    }

    fn set_current_step(&mut self, step: usize) {
        self.current_step = step;
    }

    fn get_context(&self, key: &str) -> Option<&str> {
        self.context.get(key).map(|s| s.as_str())
    }

    fn set_context(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.context.insert(key.into(), value.into());
    }

    fn token_tracker(&self) -> &crate::agent::token::TokenTracker {
        &self.token_tracker
    }
    fn token_tracker_mut(&mut self) -> &mut crate::agent::token::TokenTracker {
        &mut self.token_tracker
    }

    fn messages_mut(&mut self) -> &mut Vec<BaseMessage> {
        &mut self.messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_state_new() {
        let state = AgentState::new("/workspace");
        assert_eq!(state.cwd(), "/workspace");
        assert_eq!(state.messages().len(), 0);
        assert_eq!(state.current_step(), 0);
    }

    #[test]
    fn test_agent_state_messages() {
        let mut state = AgentState::new("/workspace");
        state.add_message(BaseMessage::human("hello"));
        state.add_message(BaseMessage::ai("hi there"));
        assert_eq!(state.messages().len(), 2);
        assert!(matches!(state.messages()[0], BaseMessage::Human { .. }));
    }

    #[test]
    fn test_agent_state_context() {
        let state = AgentState::new("/workspace")
            .with_context("key1", "value1")
            .with_context("key2", "value2");
        assert_eq!(state.get_context("key1"), Some("value1"));
        assert_eq!(state.get_context("missing"), None);
    }

    #[test]
    fn test_token_tracker_default() {
        let state = AgentState::new("/tmp");
        assert_eq!(state.token_tracker().llm_call_count, 0);
        assert_eq!(state.token_tracker().total_input_tokens, 0);
    }

    #[test]
    fn test_token_tracker_accumulate() {
        use crate::llm::types::TokenUsage;
        let mut state = AgentState::new("/tmp");
        state.token_tracker_mut().accumulate(&TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(30),
            cache_read_input_tokens: None,
        });
        assert_eq!(state.token_tracker().total_input_tokens, 100);
        assert_eq!(state.token_tracker().llm_call_count, 1);
    }
}
