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
