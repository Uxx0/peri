# peri-dag Design Review Progress

## 2026-05-04 Round 1

### 发现并修复的用户体验问题

**1. Template Run 不支持 inputs 参数（阻断级）**
后端 `run_template` API 不接受 inputs，有必填参数的模板从 UI 运行会静默失败。修复：添加 `RunTemplateRequest` 结构体，API 接受可选 JSON body 的 inputs 参数，前端在 template preview 中渲染 inputs 表单，Run 时收集并提交。

**2. Web UI 无错误反馈（高优先级）**
`runTemplate()` 的 catch 为空，用户无法得知运行失败原因。添加了 toast 通知系统，在操作成功/失败时显示提示消息。

**3. 输入类型校验缺失（中等优先级）**
`validate_inputs` 声明了 string/number/boolean 类型但不做检查。增加了 number（parse f64）和 boolean（true/false/yes/no/1/0）的类型校验，附带 8 个测试用例覆盖各种场景。

**4. CLI 无 --help（中等优先级）**
用户无法发现可用参数。添加了 `--help`/`-h` 标志，显示用法、选项和环境变量说明。

**5. Run 不显示执行耗时（中等优先级）**
UI 只显示时间戳不显示耗时。添加了 `fmtDuration()` 函数，在 run 列表和日志面板头部显示持续时间。

**6. Examples 注释中的 API 参数名错误（低优先级）**
`ci-pipeline.yaml` 注释写了 `yaml_file` 但实际 API 参数是 `yaml`，已修正。

### 测试覆盖
- 原有 16 个测试全部通过
- 新增 8 个 `validate_inputs` 类型校验测试
- 总计 24 个测试全部通过

## 2026-05-04 Round 2

### 发现并修复的用户体验问题

**1. selectRun 双重请求 + timer 泄漏（高优先级）**
`selectRun()` 先 fetch 渲染 UI，再 fetch 判断是否 poll，造成冗余请求和潜在 timer 泄漏。合并为单次 fetch：渲染后直接从响应判断是否需要轮询，同时先 clearInterval 再 fetch。

**2. 失败节点下游永远 pending（高优先级）**
节点失败后，未执行的下游节点停在 pending 状态，用户误以为还在等待。添加 `mark_run_pending_as_skipped` 方法，在 DAG 失败时将剩余 pending 节点标记为 skipped。前端同步添加 skipped 状态的颜色样式。

**3. 节点日志不显示执行耗时（中等优先级）**
日志面板每个节点 header 有 started_at 但不显示 duration。改为用 `fmtDuration()` 显示节点执行时间。

**4. 内嵌 Run 按钮绕过 inputs 表单（高优先级）**
Template 卡片上的 Run 按钮直接调用 `runTemplate()`，没经过 inputs 表单。添加 `runTemplateFromCard()`：先选中 template 展示 inputs 表单，无 inputs 则直接运行，有 inputs 让用户填写后手动点 Run。

**5. Run 列表 API 返回 yaml_content（中等优先级）**
列表查询返回完整 yaml_content 大字段但列表页用不到。修改 SQL 用空字符串替代，减少网络传输和内存占用。

**6. 三处重复 workflow 提交代码（低优先级）**
`submit_workflow`、`run_template`、`submit_workflow_from_file` 有完全相同的 run 创建+node 插入+执行启动逻辑。抽取 `create_and_start_run()` 共享函数，消除约 60 行重复代码。

## 2026-05-04 Round 3

### 发现并修复的问题

**1. PlatformFiles::resolve panic 导致服务崩溃（高优先级）**
`PlatformFiles::resolve()` 在平台不匹配时 `panic!`，直接崩溃整个进程。改为返回 `anyhow::Result<String>`，让错误沿 executor 传播为节点失败，而非服务终止。同步更新 `ScriptSource::resolve` 和 `PromptSource::resolve` 的签名。

**2. 模板卡片内联 JS 单引号注入（高优先级）**
模板名含单引号时 `onclick="showTemplatePreview('name')"` 断开 JS 字符串，导致功能崩溃甚至 XSS。改用 `data-name` 属性 + `addEventListener` 事件委托，彻底消除字符串拼接注入风险。

**3. DAG 图每次渲染重置缩放（中等优先级）**
`renderGraph` 每次都设 `dagZoom=1/dagPanX=0/dagPanY=0`，运行中的 run 刷新时用户缩放被重置。改为只在节点数变化时重置（切换 run/template），刷新更新保留缩放状态。

**4. 模板区域无标题（低优先级）**
Templates 区域没有标题 header，Runs 区域有。新用户不知道上面是什么区域。添加了 Templates 标题 header。

### 测试覆盖
- 新增 5 个 schema 测试（PlatformFiles 匹配/默认/错误、ScriptSource inline/file）
- 总计 29 个测试全部通过
