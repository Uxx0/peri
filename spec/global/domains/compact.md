# 上下文压缩增强 领域

## 领域综述

上下文压缩增强领域负责 Micro-compact 和 Full Compact 策略的全面增强，包括可压缩工具白名单、9 段结构化摘要模板和压缩后重新注入。

核心职责：
- Micro-compact 可压缩工具白名单 + 时间衰减清除策略
- Full Compact 9 段结构化摘要模板对齐 Claude Code
- 压缩后重新注入最近读取文件和激活 Skills
- 工具对完整性保护确保 tool_use + tool_result 不被拆开
- CompactConfig 通过 settings.json 配置，环境变量可覆盖

## 核心流程

### Micro-compact 流程

```
触发条件: context_usage 70%-85%
  → 白名单工具结果可压缩（bash/read/glob/search/write/edit）
  → 时间衰减: 超过 micro_compact_stale_steps(5) 步的旧结果
  → 图片替换: [image] 或 [compacted: image ~{tokens} tokens]
  → 文档替换: [document] 或 [compacted: document ~{tokens} tokens]
  → 工具对保护: adjust_index_to_preserve_invariants() 确保 tool_use + tool_result 不拆开
```

### Full Compact 流程

```
触发条件: context_usage > 85%
  → 9 段结构化摘要模板:
      Primary Request → Technical Concepts → Files → Errors & Fixes →
      Problem Solving → User Messages → Pending Tasks → Current Work → Next Step
  → 调用 LLM 生成摘要
  → 移除 <analysis> 块，保留 <summary>
  → PTL 降级重试: 按消息步数组逐步删除最旧组，最多重试 3 次
  → re_inject: 提取最近文件路径 + Skills → System 消息重新注入
```

## 技术方案总结

| 维度 | 选型 |
|------|------|
| Micro-compact | 可压缩白名单 + 时间衰减 + 图片/文档替换 + 工具对保护 |
| Full Compact | 9 段摘要模板 + LLM 调用 + PTL 降级重试 |
| 重新注入 | extract_recent_files() + extract_skills_paths() → System 消息 |
| 配置 | CompactConfig 支持环境变量覆盖 |
| 核心层分离 | 纯消息操作在核心层，TUI 层仅触发和展示 |

## Feature 附录

### feature_20260428_F001_compact-redesign
**摘要:** 全面增强 Micro/Full Compact 策略与压缩后重新注入
**关键决策:**
- Micro-compact 引入可压缩工具白名单 + 时间衰减清除策略
- Full Compact 采用 9 段结构化摘要模板对齐 Claude Code
- 压缩后重新注入最近读取文件和激活 Skills（System 消息形式）
- 工具对完整性保护：确保 tool_use 和 tool_result 不被拆开
- PTL 降级重试：按消息步数组逐步删除最旧组，最多重试 3 次
- CompactConfig 通过 settings.json 配置，环境变量可覆盖
- 核心层实现纯消息操作，TUI 层仅负责触发和 UI 展示
**归档:** [链接](../../archive/feature_20260428_F001_compact-redesign/)
**归档日期:** 2026-04-30

---

## Issue 经验附录

### issue_2026-05-11-auto-compact-no-resubmit
**摘要:** Auto Compact 后 Agent 未自动 Resubmit 继续执行
**状态:** Fixed + Verify
**归档日期:** 2026-05-13
**关键词:** last_user_input, auto-compact, resubmit, 状态保留
**问题本质:** last_user_input 在 compact 异步执行期间可能为 None 或被覆盖，导致 handle_compact_done 的 resubmit 被静默跳过，无任何日志或用户提示
**通用模式:** 跨异步操作的状态依赖（如 compact 后需要原始输入 resubmit）应在操作开始时保存到独立字段，防止异步执行期间被清理。静默跳过关键操作（如 resubmit）是危险的，应至少记录 warn 日志
**技术决策:** compact 开始时保存 last_user_input 到独立字段，防止异步期间被清理
**涉及文件:** peri-tui/src/app/agent_compact.rs, peri-tui/src/app/agent_submit.rs, peri-tui/src/app/agent_ops.rs, peri-tui/src/app/agent_comm.rs
**CLAUDE.md 链接:** false

### issue_2026-05-12-compact-auto-continue-scenarios
**摘要:** Compact 自动继续功能在不应触发的场景（手动 /compact、Done 后 auto-compact）下仍然 resubmit
**状态:** Fixed
**归档日期:** 2026-05-20
**关键词:** auto-continue, compact 触发来源, resubmit 控制, instructions 参数
**问题本质:** handle_compact_done 的 resubmit 逻辑不区分 compact 触发来源——手动 /compact 和 Done 后 auto-compact 也被错误地 resubmit。用户手动压缩后期望停下来查看结果，agent 完成任务后 compact 再用原始输入重新执行没有意义。
**通用模式:** 异步操作的触发来源（auto vs manual）需要作为上下文传递到完成后处理逻辑。用 instructions 参数区分来源，通过独立 flag（compact_should_resubmit）控制后续行为。两个合理的 resubmit 场景（auto-compact 在 agent 执行中、后台任务完成后）和两个不合理的场景（手动 compact、Done 后 compact）需要精确区分。
**涉及文件:** peri-tui/src/app/agent_compact.rs, peri-tui/src/app/agent_ops.rs, peri-tui/src/app/agent_comm.rs
**CLAUDE.md 链接:** false

---

## 相关 Feature
- → [token-tracking.md](./token-tracking.md) — Token 追踪触发压缩
- → [tui.md](./tui.md) — TUI /compact 命令
