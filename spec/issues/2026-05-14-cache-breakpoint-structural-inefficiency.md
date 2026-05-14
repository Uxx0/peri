# Prompt Cache 断点结构性效率缺陷：82% system 未缓存 + message 断点浪费

**状态**：Fixed
**优先级**：中
**创建日期**：2026-05-14

## 问题描述

分析 `msg_202605141504387ad56b6fb18b450f` 的 `cache_read_input_tokens: 0` 缓存丢失时发现两个结构性效率缺陷：(1) system prompt 中 82.6% 的内容（29,505 chars，CLAUDE.md + middleware 注入）没有 cache_control，永远无法被缓存；(2) `apply_cache_to_messages` 的第二断点策略在 tool_result-only 消息上静默失效，实际有效 message 断点仅 2 个而非设计的 3 个。

0080 请求本身的缓存丢失经逐字节对比确认是服务端瞬时驱逐（payload 前缀完全一致），但结构性问题限制了整体缓存命中率天花板。

## 症状详情

### 缓存命中率时序

| 请求 | cache_read | input | 命中率 | 间隔 |
|------|-----------|-------|--------|------|
| 0078 | 30,016 | 137 | 99.5% | — |
| 0079 | 25,600 | 4,589 | 84.8% | +15s |
| **0080** | **0** | **30,341** | **0%** | **+16s** |
| 0081 | 30,144 | 749 | 97.6% | +9s |
| 0082 | 30,336 | 4,127 | 88.0% | +5s |

### System block 结构分析

```
block[0]  6,226 chars (17.4%)  cache_control ✓  ← 仅此部分可缓存
block[1] 29,505 chars (82.6%)  cache_control ✗  ← CLAUDE.md + middleware 永不缓存
```

block[1] 内容包含 Deferred Tools 说明、SubAgent 文档、Skills 列表、CLAUDE.md 全文等跨请求稳定内容，但因通过 `messages_to_anthropic()` 归入 BOUNDARY 之后的动态段，序列化时无 cache_control。

### Message 断点失效分析

`apply_cache_to_messages` 设计 3 个 message 断点（first / second-to-last / last），但 second-to-last user message 在多轮工具调用中几乎一定是 tool_result-only 消息。`rfind(type=="text")` 返回 None，断点静默跳过：

```
0079: 2nd-to-last = msg[36] [tool_result] → cc 未生效（无 text block）
0080: 2nd-to-last = msg[37] [text]        → cc 生效，但 last=39 也是 tool_result → last 未生效
```

### 前缀对比验证

对 0079 和 0080 的请求 payload 做了逐字节对比：

| 对比项 | 结果 |
|--------|------|
| system block[0] | IDENTICAL |
| system block[1] | IDENTICAL |
| tools (14 个) | IDENTICAL |
| msg[0..37] | IDENTICAL |
| cache_control 位置 | 相同 |

0080 仅新增 msg[38] (assistant) 和 msg[39] (tool_result)，在最后一个断点 msg[37] 之后。缓存丢失确认是 Anthropic 服务端瞬时驱逐。

## 复现条件

- **复现频率**：服务端缓存丢失偶发，结构性效率问题必现
- **触发条件**：
  1. 使用 Anthropic 兼容 API，`enable_cache = true`
  2. 多轮工具调用对话（second-to-last 为 tool_result-only）
  3. system prompt 含大量 middleware 注入内容（CLAUDE.md、Skills 等）

## 修复记录

### Fix 1: `apply_cache_to_messages` 回退搜索

当目标 user 消息（second-to-last / last）无 text block 时，沿 `user_indices` 向前搜索最近的含 text block 的 user message，对其添加 cache_control。保留去重逻辑避免重复标记。

**文件**：`rust-create-agent/src/llm/anthropic.rs` `apply_cache_to_messages()`

### Fix 2: 断点重组

移除 `tools[last]` cache_control（已被 msg[first] 的缓存前缀覆盖，属于冗余断点），新增 `system[last]` cache_control（序列化时对最后一个 system block 标记）。

**变更前 4 断点**（实际有效 2-3 个）：
1. `system[0]` cc — ~2K tokens
2. `tools[last]` cc — 与断点 3 重叠
3. `msg[first]` cc — ~30K tokens
4. `msg[last]` cc — ~30K+ tokens

**变更后 4 断点**（实际有效 3-4 个）：
1. `system[0]` cc — 小粒度回退
2. `system[last]` cc — **新增**，缓存整个 system（~17K tokens）
3. `msg[first]` cc — system + tools + first user
4. `msg[second-to-last]` cc — **Fix 1 使其生效**，缓存上一轮前缀

**文件**：`rust-create-agent/src/llm/anthropic.rs`
- 删除 tools cache_control（L472-478）
- 新增 system[last] cache_control（序列化逻辑 L511-525）

## 涉及文件

- `rust-create-agent/src/llm/anthropic.rs` — `apply_cache_to_messages()`、system 序列化、tools 序列化

## 相关 Issue

- `2026-05-13-system-prompt-dynamic-cache-invalidation.md` — BOUNDARY 标记拆分（已修复，但 middleware 内容仍未缓存）
- `2026-05-13-prompt-cache-hit-rate-risks.md` — H3 断点落在 tool_result（已修复跳过逻辑，本次增强为回退搜索）
- `2026-05-13-askuserquestion-cache-hit-rate-drop.md` — 缓存下降子集表现（已修复）
