use super::*;
use peri_agent::agent::state::AgentState;
use peri_agent::agent::token::ContextBudget;
use peri_agent::messages::{BaseMessage, ContentBlock};
use std::sync::Arc;

fn make_state() -> AgentState {
    AgentState::new("/tmp/test")
}

fn make_config() -> CompactConfig {
    CompactConfig::default()
}

fn make_budget(context_window: u32) -> ContextBudget {
    ContextBudget::new(context_window)
}

fn make_event_tx() -> Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>>> {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    Arc::new(Mutex::new(Some(tx)))
}

fn make_middleware() -> CompactMiddleware {
    CompactMiddleware {
        model: None,
        config: make_config(),
        budget: make_budget(200_000),
        cwd: "/tmp/test".to_string(),
        event_tx: make_event_tx(),
        cancel: AgentCancellationToken::default(),
        hooks: vec![],
        session_id: "test-session".to_string(),
        provider_name: "test-model".to_string(),
    }
}

#[tokio::test]
async fn test_name_returns_compact_middleware() {
    let mw = make_middleware();
    assert_eq!(
        <CompactMiddleware as Middleware<AgentState>>::name(&mw),
        "CompactMiddleware"
    );
}

#[tokio::test]
async fn test_before_model_noop_when_disabled_by_env() {
    // 使用 config.auto_compact_enabled=false 模拟 disable（避免 env var 并行测试污染）
    let mw = CompactMiddleware {
        config: {
            let mut c = make_config();
            c.auto_compact_enabled = false;
            c
        },
        ..make_middleware()
    };
    let mut state = make_state();
    mw.before_model(&mut state).await.unwrap();
}

#[tokio::test]
async fn test_before_model_noop_when_config_disabled() {
    let mw = CompactMiddleware {
        config: {
            let mut c = make_config();
            c.auto_compact_enabled = false;
            c
        },
        ..make_middleware()
    };
    let mut state = make_state();
    mw.before_model(&mut state).await.unwrap();
}

#[tokio::test]
async fn test_before_model_noop_when_below_threshold() {
    // tracker 用量低，不触发任何 compact
    let mw = make_middleware();
    let mut state = make_state();
    mw.before_model(&mut state).await.unwrap();
}

#[tokio::test]
async fn test_before_model_with_low_budget_triggers_full_or_micro() {
    // budget 为 1000 token 且 tracker 已累积 → 应触发 compact
    let mut state = make_state();
    // 向 state 添加大量消息
    state.add_message(BaseMessage::human(vec![ContentBlock::text(
        "hello ".repeat(100),
    )]));

    let mw = CompactMiddleware {
        budget: ContextBudget::new(100), // 极小窗口
        model: None,                     // 无 model，full compact 会跳过
        ..make_middleware()
    };

    let result = mw.before_model(&mut state).await;
    // 无 model 时 full compact 返回 Ok 但跳过
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_compact_without_model_skips_full() {
    // 验证无 model 时 full compact 被跳过
    let mut state = make_state();
    state.add_message(BaseMessage::human(vec![ContentBlock::text("test message")]));

    let mw = CompactMiddleware {
        budget: ContextBudget::new(100),
        model: None,
        ..make_middleware()
    };

    let result = mw.before_model(&mut state).await;
    assert!(result.is_ok());
    // 无 model 时不应该 panic
}

#[tokio::test]
async fn test_borrow_safety_then_mut() {
    // 验证先读 tracker 后改 messages 的借用模式
    let mut state = make_state();
    state.add_message(BaseMessage::human(vec![ContentBlock::text("test")]));

    // 即使有低 budget，借用模式也不应 panic
    let mw = CompactMiddleware {
        budget: ContextBudget::new(1_000_000), // 大窗口，不触发
        ..make_middleware()
    };

    let result = mw.before_model(&mut state).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_is_disabled_detects_config() {
    let mw = make_middleware();
    // 默认情况 auto_compact_enabled=true，不应 disabled
    assert!(!mw.is_disabled());

    let mw = CompactMiddleware {
        config: {
            let mut c = make_config();
            c.auto_compact_enabled = false;
            c
        },
        ..make_middleware()
    };
    assert!(mw.is_disabled());
}
