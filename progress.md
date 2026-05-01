# Design Review Progress

## 2026-05-02 第31轮：核心框架 Code Review 与去重优化

审查 rust-create-agent 核心框架（executor/chain/LLM 适配层/state），合并 executor 中重复的 should_warn 调用和 pct 阈值判断；为 ChatAnthropic 显式声明 context_window；删除 grep.rs 中 115 行未使用的 parse_args 死代码；为 StopReason 补充 9 个单元测试。504 测试全通过。测试总数 293。

## 2026-04-30 第30轮 — 第21轮：UX 打磨与 Bug 修复

| 轮 | 改动 | 测试 |
|---|---|---|
| 30 | Thread Browser 空列表添加引导提示 | 290 |
| 29 | Thread Browser 新建对话添加反馈消息 | 290 |
| 28 | 修复 Agent/Cron 面板描述截断的字节/字符混淆（.len→.chars().count） | 290 |
| 27 | Thread Browser 确认删除时面板高度不足导致提示被截断 | 290 |
| 26 | Thread Browser Ctrl+D 删除从立即执行改为两步确认 | 290 |
| 25 | Cron 面板关闭时清理 panel_selection/panel_area；Setup wizard 错误消息中文化 | 290 |
| 24 | Welcome Card 新增 Provider/Model 信息行；Thread Browser 对话列表追加消息数量标签 | 290 |
| 23 | Model/Login 面板操作成功反馈（切换模型、激活 Provider、保存） | 289 |
| 22 | Model 面板 Space 选中模型；Cron 删除增加确认步骤；面板粘贴事件统一拦截 | 287 |
| 21 | Cron 缓冲消息改为逐条发送，避免多个 cron 任务被合并为一条消息 | 833 |

## 2026-04-30 第20轮 — 第14轮：核心逻辑审查与优化

| 轮 | 改动 | 测试 |
|---|---|---|
| 20 | RetryableLLM 消除不可达死代码，BashTool 超时 clamp(1,300) | 833 |
| 19 | ContextBudget 事件链路：executor 新增 ContextWarning 事件发出 | 829 |
| 18 | LLM 适配层 context_window() 精确模型名推断（不再硬编码前缀匹配） | 826 |
| 17 | Anthropic Prompt Caching 改为在第一条 user 消息上加 cache_control（稳定缓存边界） | 823 |
| 16 | ContextBudget 定义层与执行层脱节修复：executor 改用 ContextBudget::should_warn() | 818 |
| 15 | SubAgent 消除二重文件解析冗余 I/O；新增 cancel 令牌传递链路支持 Ctrl+C 中断子 agent | 816 |
| 14 | HITL 批量审批：新增 before_tools_batch 钩子，多个敏感工具合并为一次审批弹窗 | 812 |

## 2026-04-29 第1轮 — 第13轮：初始 UX 全面审查

| 轮 | 改动 | 测试 |
|---|---|---|
| 13 | 清理 Tips 中引用不存在命令的提示（/rename 等 6 条），新增回归测试 | 252 |
| 12 | /compact 防重复触发；spinner 文字提示；micro-compact 消息中文化 | 786 |
| 11 | ToolBlock 错误结果 ERROR 红色高亮；/help 补全局快捷键提示 | 784 |
| 10 | 系统消息颜色按内容自动分级（错误红/警告橙/普通绿）；/compact 即时反馈 | 784 |
| 9 | 未配置 Provider 错误消息改为引导文案；状态栏显示任务运行时长 | 784 |
| 8 | 输入框占位提示(Alt+Enter换行)；命令前缀多匹配显示候选列表；状态栏快捷键提示 | 250 |
| 7 | Thread Browser 当前对话 ✓ 标识；ToolCallGroup 折叠展开提示；/help 补 Skills 说明 | 247 |
| 6 | Welcome Card 未配置引导；命令栏精简；工具运行中文字标签 | 247 |
| 5 | Cron 空列表引导和删除反馈；Login 编辑模式 Ctrl+V 提示和保存错误反馈 | 246 |
| 4 | Agent 面板空列表添加引导；Model 面板未配置时显示 /login 引导 | 244 |
| 3 | 全面排查单字母快捷键违规：HITL 改 Space+Enter，删除改 Ctrl+D，编辑改 Ctrl+N | 241 |
| 2 | Cron 面板 d 键删除修复；Thread Browser 删除后反馈消息 | 772 |
| 1 | Thread Browser/Login 面板删除功能缺失修复；Welcome Card 快捷键提示；配置保存错误反馈 | 772 |
