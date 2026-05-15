# 多工具并发执行中途出错导致孤儿 tool_use 触发 Anthropic API 400

**状态**：Open
**优先级**：高
**创建日期**：2026-05-15

## 问题描述

LLM 并发调用多个工具时，其中一个工具执行出错导致 `tool_dispatch.rs` 提前退出，已写入 state 的 AI 消息（含 `tool_use` blocks）没有对应的 `tool_result` blocks。下一轮 API 请求将这条不完整的消息序列发送给 Anthropic，触发 400 错误：`tool_use ids were found without tool_result blocks immediately after`。

## 症状详情

```
LLM HTTP 错误 (400): API 错误 400 Bad Request: messages.15: `tool_use` ids were found
without `tool_result` blocks immediately after: call_00_LahGsr8aIx8ZhtqTqOPW3747.
Each `tool_use` block must have a corresponding `tool_result` block in the next message.
```

### 触发场景

- LLM 同时发起多个工具调用（如 Read + Grep + Glob）
- 其中某个工具在执行阶段报错
- `tool_dispatch.rs` 的错误处理路径未完全 flush 所有 pending 的 tool_result
- 下一轮 API 请求携带不完整消息 → Anthropic 拒绝

### 复现条件

- **复现频率**：偶发（非每次工具出错都触发，取决于并发工具数量和出错时机）
- **触发步骤**：
  1. 使用 Anthropic API（直接或兼容端口）
  2. 触发 LLM 并发调用多个工具
  3. 其中一个工具执行报错（如文件不存在、权限不足等）
  4. Agent 尝试继续下一轮 → API 400
- **环境**：Anthropic API，多工具并发场景

## 相关代码

`peri-agent/src/agent/executor/tool_dispatch.rs` 采用两阶段写入模式：

1. **阶段一**（约第 37 行）：AI 消息（含所有 `tool_use` blocks）写入 state
2. **阶段二**：`before_tool` 循环 + 并发执行
3. **阶段三**（约第 253 行）：`tool_result` blocks 写入 state

阶段一和阶段三之间存在多个提前退出路径（P1 cancel / P3 ToolRejected / P4 其他错误），这些路径必须同时 flush：
- `modified_calls`（已通过 before_tool 但未执行的工具）→ `flush_modified_tool_errors`
- `original_calls[i..]`（尚未进入 before_tool 的工具）→ `flush_pending_tool_errors`

CLAUDE.md 记录此模块已因此 bug 修复 4 次（f138b21, 7f3ad00, 8d6bb1b 及更早）。根本原因是两阶段写入架构本身容易遗漏 flush 路径。

## 历史修复记录

| 提交 | 修复内容 |
|------|----------|
| f138b21 | flush 路径修复 |
| 7f3ad00 | flush 路径修复 |
| 8d6bb1b | flush 路径修复 |
| 更早 | 初始修复 |

## 关联 Issue

- `spec/issues/2026-05-15-tool-execution-error-stops-agent.md`（Fixed，部分修复）—— after_tool 中间件错误设 deferred_error 导致 Agent 停止，与孤儿 tool_use 是同一模块的不同错误路径
- `spec/issues/2026-05-14-deepseek-multi-turn-tool-result-duplication.md`（Fixed）—— 不同根因（StateSnapshot 重复）但同样导致 tool_use/tool_result 不匹配
