use super::*;

#[test]
fn test_context_warning_serde_roundtrip() {
    let ev = AgentEvent::ContextWarning {
        used_tokens: 150000,
        total_tokens: 200000,
        percentage: 75.0,
    };
    let json = serde_json::to_string(&ev).unwrap();
    let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
    if let AgentEvent::ContextWarning {
        used_tokens,
        total_tokens,
        percentage,
    } = deserialized
    {
        assert_eq!(used_tokens, 150000);
        assert_eq!(total_tokens, 200000);
        assert!((percentage - 75.0).abs() < 0.01);
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_llm_retrying_serde_roundtrip() {
    let ev = AgentEvent::LlmRetrying {
        attempt: 2,
        max_attempts: 5,
        delay_ms: 2000,
        error: "API 错误 503: Service Unavailable".to_string(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
    if let AgentEvent::LlmRetrying {
        attempt,
        max_attempts,
        delay_ms,
        error,
    } = deserialized
    {
        assert_eq!(attempt, 2);
        assert_eq!(max_attempts, 5);
        assert_eq!(delay_ms, 2000);
        assert_eq!(error, "API 错误 503: Service Unavailable");
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_subagent_started_serde_roundtrip() {
    let ev = AgentEvent::SubagentStarted {
        agent_name: "test-agent".to_string(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""type":"subagent_started""#));
    assert!(json.contains(r#""agent_name":"test-agent""#));
    let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
    if let AgentEvent::SubagentStarted { agent_name } = deserialized {
        assert_eq!(agent_name, "test-agent");
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_subagent_stopped_serde_roundtrip() {
    let ev = AgentEvent::SubagentStopped {
        agent_name: "test-agent".to_string(),
        result: "done".to_string(),
        is_error: false,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""type":"subagent_stopped""#));
    let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
    if let AgentEvent::SubagentStopped {
        agent_name,
        result,
        is_error,
    } = deserialized
    {
        assert_eq!(agent_name, "test-agent");
        assert_eq!(result, "done");
        assert!(!is_error);
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_session_ended_serde() {
    let ev = AgentEvent::SessionEnded;
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""type":"session_ended""#));
    let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(deserialized, AgentEvent::SessionEnded));
}

#[test]
fn test_compact_started_serde() {
    let ev = AgentEvent::CompactStarted;
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""type":"compact_started""#));
    let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(deserialized, AgentEvent::CompactStarted));
}

#[test]
fn test_compact_completed_serde_roundtrip() {
    // full compact 场景：summary 非空，micro_cleared 为 0
    let ev = AgentEvent::CompactCompleted {
        summary: "对话摘要内容".to_string(),
        files: vec![
            CompactFileInfo {
                path: "src/main.rs".to_string(),
                lines: 42,
            },
            CompactFileInfo {
                path: "src/lib.rs".to_string(),
                lines: 15,
            },
        ],
        skills: vec!["code-review".to_string(), "refactor".to_string()],
        micro_cleared: 0,
        messages: vec![],
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""type":"compact_completed""#));
    assert!(json.contains(r#""summary":"对话摘要内容""#));
    assert!(json.contains(r#""path":"src/main.rs""#));
    assert!(json.contains(r#""skills":["code-review","refactor"]"#));
    let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
    if let AgentEvent::CompactCompleted {
        summary,
        files,
        skills,
        micro_cleared,
        messages: _,
    } = deserialized
    {
        assert_eq!(summary, "对话摘要内容");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].lines, 42);
        assert_eq!(files[1].path, "src/lib.rs");
        assert_eq!(files[1].lines, 15);
        assert_eq!(skills, vec!["code-review", "refactor"]);
        assert_eq!(micro_cleared, 0);
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_compact_completed_micro_serde() {
    // micro-compact 场景：summary 为空，micro_cleared > 0
    let ev = AgentEvent::CompactCompleted {
        summary: String::new(),
        files: vec![],
        skills: vec![],
        micro_cleared: 8,
        messages: vec![],
    };
    let json = serde_json::to_string(&ev).unwrap();
    let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
    if let AgentEvent::CompactCompleted {
        summary,
        files,
        skills,
        micro_cleared,
        messages: _,
    } = deserialized
    {
        assert!(summary.is_empty());
        assert!(files.is_empty());
        assert!(skills.is_empty());
        assert_eq!(micro_cleared, 8);
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_compact_error_serde_roundtrip() {
    let ev = AgentEvent::CompactError {
        message: "LLM 调用超时".to_string(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""type":"compact_error""#));
    assert!(json.contains(r#""message":"LLM 调用超时""#));
    let deserialized: AgentEvent = serde_json::from_str(&json).unwrap();
    if let AgentEvent::CompactError { message } = deserialized {
        assert_eq!(message, "LLM 调用超时");
    } else {
        panic!("Deserialized to wrong variant");
    }
}
