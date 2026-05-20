# ACP 领域

## 领域综述

ACP（Agent Client Protocol）领域负责通过 stdio 传输为 IDE（如 Cursor）提供 Agent 服务端能力。

核心职责：

- Session 生命周期：initialize、new、load、resume、cancel、logout、close
- 请求处理：RequestPermission（HITL 桥接）、$/cancel_request（单请求取消）
- 更新推送：AvailableCommandsUpdate、SessionNotification 事件流
- Agent 构建复用：与 TUI 共享 build_bare_agent() 入口

## 核心流程

```
ACP Client (IDE) → stdio → handle_initialize/session/new/load...
  → assemble_agent() → executor.execute() → SessionNotification 流
  → RequestPermission RPC → AcpInteractionBroker → HITL 审批桥接
  → $/cancel_request → oneshot cancel → 中断 pending request
```

## 技术方案总结

| 维度 | 选型 |
|------|------|
| 传输层 | stdio（stdin/stdout），JSON-RPC 2.0 |
| Session 管理 | AcpSession，DashMap<SessionId, ...>，支持多 session |
| Agent 构建 | build_bare_agent() 共享入口，中间件链一致 |
| 权限桥接 | AcpInteractionBroker 实现 UserInteractionBroker trait |
| Pending Request | DashMap<RequestId, PendingRequestEntry> + oneshot::Sender |
| 命令推送 | AvailableCommandsUpdate，在 session/new/load/resume 三个入口统一发送 |

## Issue 经验附录

### issue_2026-05-16-acp-cancel-request-unimplemented

**摘要:** ACP 未实现 $/cancel_request 与 AvailableCommandsUpdate
**状态:** Closed
**归档日期:** 2026-05-17
**关键词:** ACP, cancel_request, oneshot, pending requests, AvailableCommandsUpdate
**问题本质:** ACP 协议实现中的两个缺口：(1) $/cancel_request 通知未处理导致无法取消单个请求；(2) AvailableCommandsUpdate 从未发送导致 IDE 端命令补全不可用
**通用模式:** 协议级通知（notification）与请求（request）是独立通道，废弃的 notification handler 会静默丢弃客户端通知；pending request 追踪需要支持一对一取消（oneshot channel）而非仅全局取消
**技术决策:** DashMap<RequestId, PendingRequestEntry> 追踪 pending requests，oneshot::Sender<()> 实现单请求取消；build_available_commands() 在 session/new、load、resume 三个入口统一发送
**涉及文件:** peri-tui/src/acp/dispatch.rs
**CLAUDE.md 链接:** false

### issue_2026-05-17-acp-dispatch-heavy-file

**摘要:** ACP dispatch.rs 请求分发逻辑过度集中（1044 行），14 个 pub 函数和所有 handler 实现集中在一个文件
**状态:** Fixed
**归档日期:** 2026-05-18
**涉及文件:** peri-tui/src/acp/dispatch.rs
**说明:** 纯代码组织优化——按 handler 分组拆分为子目录（initialize/session/prompt/permission/helpers），无领域认知提炼。

### issue_2026-05-19-acp-missing-capabilities

**摘要:** ACP 协议能力缺失：Session 生命周期路由、能力声明、SessionUpdate 通知变体
**状态:** Fixed
**归档日期:** 2026-05-20
**关键词:** Session 生命周期, ACP 路由, 能力声明, SessionUpdate
**问题本质:** TUI ACP Server 的路由层未接通——SessionManager 已有完整基础设施，但 acp_server.rs 缺少 session/load/resume/close/list/fork 路由处理，且 InitializeResponse 的能力声明为空。这是典型的"底层能力已存在但协议层未暴露"的分层架构问题。
**通用模式:** 协议层路由是基础设施能力的"出口"。当 SessionManager 支持了某操作但 ACP handler 无路由时，外部客户端完全无法使用。新增基础设施能力时，需同步检查所有协议/传输层的路由表是否齐备。
**架构影响:** ACP 协议中 fs/terminal 代理是面向远程 Agent 场景的——本地 Agent（perihelion）通过 FilesystemMiddleware 直接操作 std::fs 是正确的架构选择，仅在 stdio 连接外部 IDE 时才有实现意义。
**涉及文件:** peri-tui/src/acp_server.rs, peri-acp/src/event/mapper.rs, peri-acp/src/session/state_builders.rs
**CLAUDE.md 链接:** false

### issue_2026-05-19-acp-stopreason-hardcoded-endturn

**摘要:** ACP PromptResponse 的 StopReason 全部硬编码为 EndTurn，无法区分取消/超限/正常完成
**状态:** Fixed
**归档日期:** 2026-05-20
**关键词:** StopReason, PromptResult, 枚举映射, 终止原因
**问题本质:** executor 返回的 PromptResult 只携带 ok: bool，不包含终止原因的语义信息。协议边界处的语义压缩（正常完成/取消/超限 → 统一 EndTurn）导致 IDE 客户端无法给出差异化 UI 反馈。
**通用模式:** 内部错误类型的语义信息应在跨越协议/API 边界时保有足够粒度。使用 bool 做结果标识会导致语义坍缩——应使用枚举携带区分性的终止原因。新增 PromptStopReason 中间层枚举是解耦内部 AgentError 和外部 ACP StopReason 的正确模式。
**涉及文件:** peri-acp/src/session/executor.rs, peri-tui/src/acp_server.rs, peri-tui/src/acp_stdio.rs
**CLAUDE.md 链接:** false

### issue_2026-05-19-acp-missing-state-notifications

**摘要:** ACP 状态变更后不发通知：ConfigOptionUpdate / AvailableCommandsUpdate / SessionInfoUpdate
**状态:** Resolved
**归档日期:** 2026-05-20
**关键词:** SessionUpdate, ConfigOptionUpdate, 状态同步, 多客户端
**问题本质:** set_mode/set_config_option/set_model 等请求处理器变更状态后只返回 response，不推送 session/update 通知。多客户端场景下其他客户端无法感知变更，UI 控件不同步。
**通用模式:** 任何改变共享状态的请求处理器，在返回 response 后必须主动推送状态变更通知。这是"写操作 + 广播通知"模式——状态变更方可能不是 UI 更新方。遵循 CQRS 思想：命令处理完成后推送事件通知所有订阅者。
**涉及文件:** peri-tui/src/acp_server.rs, peri-tui/src/acp_stdio.rs, peri-acp/src/event/mapper.rs, peri-acp/src/session/event_sink.rs
**CLAUDE.md 链接:** false

---

## 相关 Feature

- → [tui.md](./tui.md) — ACP 与 TUI 共享 build_bare_agent() Agent 构建入口
