# Prompt Cache 命中率下降风险报告

**日期**：2026-05-13
**范围**：system prompt、tool 定义、消息序列化、context compression、middleware 注入全链路
**基线**：`__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` 修复后（`spec/issues/2026-05-13-system-prompt-dynamic-cache-invalidation.md`）

---

## 风险总览

| # | 问题 | 风险 | 可修复性 |
|---|------|------|----------|
| H1 | `resource_summary()` HashMap 迭代顺序不确定 | ✅ 已排除 | — |
| H2 | MCP/LSP 工具数量跨进程不稳定 | ✅ 已排除 | — |
| H3 | `apply_cache_to_messages` 断点落在 tool_result 上 | ✅ 已修复 | — |
| H4 | Micro-compact 修改 cache 断点之前的消息 | 🔴 高 | 中等 |

> 排除项：Full Compact 完全破坏缓存是已知架构取舍（compact 后消息结构必然重建，非 bug）；LLM 摘要不确定性是模型固有特性，不可修复。

---

## H1. `resource_summary()` HashMap 迭代顺序不确定

**文件**：`rust-agent-middlewares/src/mcp/client.rs:867-884`

**现象**：`resource_summary()` 遍历 `self.clients.read().values()`，对每个 client 的 `resources` 也做 `.iter()` 遍历。两层 HashMap 迭代均无排序。即使 MCP 资源列表在进程间完全不变，Rust HashMap 的随机化种子可能导致 server 和 resource URI 的输出顺序跨进程不同。

**影响链**：
1. `McpResourceTool::new()` 调用 `resource_summary()` 构建 `cached_description`（`resource_tool.rs:37`）
2. 描述变化 → `mcp_read_resource` 工具定义变化
3. 工具定义位于 system prompt 之后、messages 之前，属于 prompt cache 前缀的一部分
4. 前缀变化 → 整个缓存失效

**触发条件**：每次进程重启（HashMap seed 随机化），即使 MCP 服务器和资源完全不变。

**复现概率**：取决于 HashMap 内部状态，非 100% 但在多 server / 多 resource 场景下概率较高。

**排除记录**（2026-05-13）：`McpResourceTool`（名称 `mcp_read_resource`）被 `is_deferred_tool()` 过滤（`agent.rs:317` + `tool_search/core_tools.rs:57`），不参与 LLM 可见的 tools 数组。`resource_summary()` 的输出从不会序列化到 API 请求中，顺序不确定不影响缓存前缀。

---

## H2. MCP/LSP 工具数量跨进程不稳定

**文件**：
- `rust-agent-middlewares/src/mcp/middleware.rs:28-36`
- `rust-agent-middlewares/src/lsp/middleware.rs:42-47`

**现象**：`McpMiddleware::collect_tools()` 和 `LspMiddleware::collect_tools()` 根据运行时连接状态决定工具数量：
- MCP：`build_tool_bridges(&self.pool)` 只返回 `ClientStatus::Connected` 的客户端工具；`has_resources()` 决定是否追加 `McpResourceTool`
- LSP：`!self.pool.has_servers()` 时返回空列表

**影响链**：
1. MCP 服务器启动超时 / 进程重启后某 server 未连上 → 工具数量减少
2. 工具数量变化 → tools 数组长度变化 → 前缀长度变化
3. Anthropic prompt cache 基于前缀匹配，长度变化 = 前缀不匹配 → 全量缓存失效

**触发条件**：MCP 服务器连接失败、LSP 服务器启动慢、进程重启时网络抖动。

**排除记录**（2026-05-13）：所有 `mcp__*` 工具和 LSP 工具均被 `is_deferred_tool()` 过滤（`agent.rs:317`），从 `tool_refs` 中移除（`executor/mod.rs:214`），不参与序列化到 API 请求的 tools 数组。连接状态变化不影响缓存前缀。

---

## H3. `apply_cache_to_messages` 断点落在 tool_result 上

**文件**：`rust-create-agent/src/llm/anthropic.rs:304-372`

**现象**：3 断点策略（第一条 / 倒数第二条 / 最后一条 user 消息）中，第 3 断点（最后一条 user 消息的最后一个 block）经常落在 `tool_result` 类型 block 上。

典型场景：用户提问 → agent 调用 Read 工具 → tool_result 返回文件内容 → 同轮第二次 LLM 调用。此时最后一条 user 消息仅含 tool_result blocks，`apply_cache_to_messages` 将 `cache_control` 加在 tool_result 的最后一个 block 上。

**影响链**：
1. tool_result 内容跨请求不同（每次工具输出不同）
2. 断点在不稳定内容上 → 该断点之后的缓存段无法命中
3. 第 3 断点的目的是"同轮内多次工具调用间复用缓存"，但 tool_result 变化使其完全失效
4. 浪费 cache write 成本（1.25x 写入 multiplier）

**触发条件**：任何工具调用后的同轮后续 LLM 请求。在多工具并发场景（agent 一次调用 3-5 个工具）中尤其严重。

**修复记录**（2026-05-13）：断点选择逻辑从"取最后一个 block"改为"从后向前搜索最后一个非空 text block"（`rfind`）。若 user 消息全是 `tool_result`/`tool_use` block，跳过该断点。新增 4 个测试覆盖：跳过尾部 tool_result、跳过 tool_use+tool_result、全 tool block 跳过、多 user 消息三断点正确放置。

---

## H4. Micro-compact 修改 cache 断点之前的消息

**文件**：`rust-create-agent/src/agent/compact/micro.rs:78-195`

**现象**：Micro-compact 在 token 达到 70% 时触发，将超过 `micro_compact_stale_steps`（默认 5）步的旧 Tool 消息内容替换为 `[compacted: N chars]` 占位符。

**影响链**：
1. `apply_cache_to_messages` 的第 1 断点（第一条 user 消息）指向对话最初的消息
2. 若第一条 user 消息中包含 tool_result（例如用户上传了文件 → agent 读取），且该 round 超过 stale threshold
3. Micro-compact 将 tool_result 替换为 `[compacted: N chars]` → 消息内容变化
4. 第 1 断点之后的所有缓存失效 → 整个前缀缓存被破坏

**触发条件**：长对话（>5 轮）中，第一条 user 消息包含工具结果。

**缓解因素**：`stale_threshold` 默认 5，最近 5 轮不受影响。如果对话不长，或第一条 user 消息在最近 5 轮内，缓存不受影响。

**修复建议**：
- 方案 A：Micro-compact 跳过第 1 断点对应的 user 消息范围内的 Tool 消息（需知道断点位置）
- 方案 B：将 `stale_threshold` 与第 1 断点位置对齐，保证断点之前的消息永不被 compact
- 方案 C：Micro-compact 后主动 `request_rebuild()`，让 pipeline 重新计算断点位置（成本最低，但仅缓解不根治）

---

## 附录：已确认安全的路径

| 路径 | 状态 | 原因 |
|------|------|------|
| 核心工具描述（12 个） | ✅ 静态 | 全部 `const &str` |
| 工具列表排序 | ✅ 稳定 | `sort_by_key(\|t\| t.name())`（`executor/mod.rs:223`） |
| 静态段落 01-06 | ✅ 无动态占位符 | 无 `{{` 模式 |
| `prepend_message` LIFO 顺序 | ✅ boundary 位置正确 | `with_system_prompt` 最后执行，排 index 0 |
| AgentsMd / Skills 中间件注入 | ✅ 在 boundary 之后 | prepend 被挤到 boundary 之后，不影响静态缓存段 |
| Deferred tools 提示词 | ✅ 缓存 + 排序 | `cached_prompt` + `sort_by_key` |
| `ReactLLM` / `LlmRequest` 接口 | ✅ 零变更 | `SystemPromptBlock` 封装在 `anthropic.rs` 内部 |
