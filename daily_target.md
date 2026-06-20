# Staff Coding CLI Tool MVP Daily Target

## 目标定义

构建一个 **最小可用、可自动执行、无需人工介入的本地终端 Coding CLI Tool：`staff`**。

MVP 不优先做完整 TUI，也不先做云端 PR Agent。第一阶段只要求 headless CLI 跑通完整闭环：

```bash
staff exec --auto "<coding task>"
```

它必须能完成：

- 读取项目上下文：`AGENTS.md`、repo map、相关文件片段、历史摘要。
- 调用模型：DeepSeek OpenAI-compatible provider；验收测试必须真实调用 `deepseek-v4-pro`。
- 使用受控工具：read、search、write、apply_patch、shell、git。
- 自动执行安全动作：workspace 内读写和常见测试命令可执行。
- 自动拒绝危险动作：工作区外写入、敏感文件、危险 shell、MCP side effect 默认 deny，不等待人工审批。
- 写文件前创建 checkpoint，长输出进入 artifact。
- 每次运行生成可观测记录：events、summary、artifacts、failure category。
- 通过 eval runner 证明最小 coding 任务可完成。

## MVP 验收标准

- `staff help`、`staff tools`、`staff sandbox doctor` 可运行。
- `staff exec --auto "<task>"` 能在 fixture repo 中完成一次小代码修改。
- 支持 DeepSeek OpenAI-compatible provider：`base_url`、`api_key_env`、`model` 可配置。
- 支持从工作区根目录 `.staff/config.toml` 和 `.staff/ds-sk` 读取 DeepSeek 配置，像 Codex 一样把本地配置放在根目录配置文件夹下。
- 验收测试、Gate smoke、golden eval 必须真实调用 DeepSeek，不允许用 mock/fake-provider 作为通过依据。
- 工具调用统一经过 permission engine 和 tool registry。
- 写文件或 patch 前生成 checkpoint，编辑后生成 diff artifact。
- shell 长输出不会进入主上下文，只进入 `.staff/artifacts/`。
- 每次运行生成 `.staff/runs/<run_id>/events.jsonl` 和 `summary.md`。
- `staff runs show latest` 能展示最近一次运行的摘要、工具时间线、变更、测试结果和失败归因。
- 至少包含 3 个 eval fixtures，并输出 `scorecard.json`。
- 工程回归全绿；核心 golden eval 平均分不低于 8/11。

## 阶段 Gate

### Gate 1：hello-world-write smoke

完成到 **Phase 5** 后必须能通过第一个最小冒烟测试：

```bash
staff exec --auto "Write a hello world program in this repo"
staff runs show latest
```

Gate 1 验收标准：

- 文件确实被创建或修改。
- 内容包含可运行的 hello world 程序。
- 全程无需人工审批。
- 写入只发生在 workspace 内。
- 写入前生成 checkpoint。
- 写入后生成 diff artifact。
- `.staff/runs/<run_id>/events.jsonl` 存在。
- `.staff/runs/<run_id>/summary.md` 存在。
- `staff runs show latest` 能看到本次变更、checkpoint、diff 和最终摘要。

Gate 1 必须真实调用 DeepSeek `deepseek-v4-pro` 生成 tool call；它的目标是验证 CLI、runtime、provider、tool registry、permission、checkpoint、diff、runs observability 的最小闭环。

### Gate 2：safe-shell verification smoke

完成到 **Phase 6** 后，hello world 冒烟测试必须额外支持安全验证命令：

```bash
staff exec --auto "Write a hello world program and run the project test command"
staff runs show latest
```

Gate 2 验收标准：

- Gate 1 全部通过。
- 安全 shell 命令可执行。
- 危险 shell 命令会自动 deny。
- summary 中能看到测试命令、测试输出摘要和失败分类。
- 长测试输出进入 artifact，不污染主上下文。

### Gate 3：eval smoke

完成到 **Phase 8** 后必须能跑稳定评测：

```bash
staff eval run --suite smoke
```

Gate 3 验收标准：

- 至少 3 个 fixture 可重复运行。
- 输出 eval events、scorecard、summary。
- 真实 DeepSeek 调用可完成评测；失败 case 有可观测归因，不允许用 mock 成绩替代。
- 平均分可计算，失败 case 有 failure category。

## 默认策略

- 产品形态：本地终端 CLI，先 headless，后 TUI。
- 默认命令：`staff exec --auto "<task>"`。
- 默认权限：`workspace-write + auto-safe + deny-dangerous`。
- 默认模型：DeepSeek OpenAI-compatible provider，模型默认 `deepseek-v4-pro`。
- 默认多 agent：MVP 可以先单主线程；`explore/review/verify` 作为第一个增强点。
- 默认沙箱：MVP 先实现 permission gate 和 sandbox doctor，不强制 OS 级隔离。
- 默认可观测：所有 model/tool/permission/checkpoint/artifact/eval 都必须写事件。

## 模块任务

### 1. CLI 与配置

- 用 `clap` 管理 CLI 参数。
- 保留命令：
  - `staff`
  - `staff exec --auto "<prompt>"`
  - `staff resume [thread_id]`
  - `staff tools`
  - `staff sandbox doctor`
  - `staff runs list/show/timeline/artifacts/failures`
  - `staff eval run/compare`
- 用 `serde + toml + serde_json` 替换手写配置解析。
- 支持 `staff.toml`：

```toml
model = "deepseek-v4-pro"
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"
mode = "auto"
sandbox = "workspace-write"
approval_policy = "deny-dangerous"
max_subagents = 3
context_budget_tokens = 64000
mcp_config = ".staff/mcp.json"
```

DeepSeek provider 也可放在工作区根目录的 `.staff/config.toml`：

```toml
model = "deepseek-v4-pro"
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"
api_key_file = ".staff/ds-sk"
```

读取优先级：

- `api_key_env` 指定的环境变量。
- `DEEPSEEK_API_KEY`。
- `.staff/config.toml` 中的 `api_key_file`。
- 默认 `.staff/ds-sk`。

### 2. Runtime 与持久化

- 统一 `Thread -> Turn -> Item` 事件模型。
- 所有 `exec`、未来 TUI、eval runner 都消费同一 runtime。
- 保存到：
  - `.staff/threads/<thread_id>.jsonl`
  - `.staff/runs/<run_id>/events.jsonl`
  - `.staff/runs/<run_id>/summary.md`
- `staff resume latest` 可恢复最近线程。

### 3. LLM Provider

- 定义 `ModelProvider` trait。
- 内置：
  - `DeepSeekProvider`：基于 OpenAI-compatible `/chat/completions`，用于所有验收测试。
  - `FakeProvider`：仅用于离线开发和单元测试，不计入验收通过。
- MVP 支持非 streaming。
- 模型响应必须能表达：
  - 普通文本回答。
  - tool call 请求。
  - final summary。

### 4. Tool Registry

- 每个 tool 必须声明：
  - name
  - input schema
  - permission action
  - output strategy
  - artifact strategy
- MVP 内置工具：
  - `read`
  - `search`
  - `write`
  - `apply_patch`
  - `shell`
  - `git_status`
  - `git_diff`
  - `checkpoint`
  - `lsp_diagnostics` 占位
- 长输出默认落 artifact，只把摘要回灌主上下文。

### 5. Permission 与自动模式

- `--auto` 下不弹窗。
- 安全动作自动 allow：
  - 工作区内读文件。
  - 工作区内写文件。
  - `rg`、`git status`、`git diff`、`cargo test`、`npm test`、`pnpm test` 等常见命令。
- 危险动作自动 deny：
  - 读 `.env`、`~/.ssh` 等敏感路径。
  - 写工作区外路径。
  - `rm -rf`、`sudo`、`curl | sh`、`git push` 等危险命令。
  - MCP side-effect tool。
- 每次权限决策写入 observability event。

### 6. Context 与 Artifact

- ContextPack 保留：
  - goal
  - constraints
  - decisions
  - evidence
  - next_steps
  - artifact summaries
- 加载来源：
  - `AGENTS.md`
  - repo map
  - 相关文件片段
  - 历史 compaction
  - `.staff/skills/*/SKILL.md` 摘要
- 长日志、搜索结果、测试输出写入 `.staff/artifacts/`。
- artifact 记录 sha256、bytes、kind、summary、path。

### 7. Checkpoint 与 Diff

- 所有写入前创建 checkpoint。
- 写入后生成 diff artifact。
- summary 中列出：
  - changed files
  - checkpoint ids
  - diff artifact paths
- 支持恢复：

```bash
staff checkpoint restore <checkpoint_id>
```

### 8. Observability

每次 `staff exec --auto` 必须生成：

```text
.staff/runs/<run_id>/events.jsonl
.staff/runs/<run_id>/summary.md
```

必须记录事件：

- `run.started`
- `run.completed`
- `run.failed`
- `context.built`
- `context.compacted`
- `context.artifact_created`
- `model.requested`
- `model.completed`
- `model.failed`
- `tool.requested`
- `tool.permission_decided`
- `tool.started`
- `tool.completed`
- `tool.failed`
- `file.checkpoint_created`
- `file.diff_created`
- `file.restored`
- `subagent.started`
- `subagent.completed`
- `subagent.failed`
- `eval.started`
- `eval.case_completed`
- `eval.completed`

每次 run 聚合指标：

- `duration_ms`
- `model_calls`
- `tool_calls`
- `permission_allows`
- `permission_denies`
- `artifacts_created`
- `files_changed`
- `checkpoints_created`
- `tests_run`
- `tests_passed`
- `tokens_in`
- `tokens_out`
- `estimated_cost`
- `failure_category`

失败分类：

- `model_error`
- `tool_error`
- `permission_denied`
- `context_missing`
- `test_failed`
- `patch_failed`
- `sandbox_error`
- `config_error`
- `unknown`

### 9. Eval Runner

建立本地评测目录：

```text
evals/
  fixtures/
    fix_small_bug/
    add_cli_option/
    update_config_parser/
  suites/
    smoke.toml
```

每个 fixture 包含：

- 初始 repo。
- 用户 prompt。
- 允许工具。
- 权限模式。
- 期望文件变更。
- 期望测试命令。
- 评分规则。

评分卡总分 11：

| 指标 | 分值 |
| --- | --- |
| Task success | 0/1 |
| Tests passed | 0/1 |
| Diff minimality | 0-2 |
| Permission safety | 0-2 |
| Context hygiene | 0-2 |
| Evidence quality | 0-2 |
| Recovery behavior | 0/1 |

输出：

```text
.staff/evals/<eval_run_id>/events.jsonl
.staff/evals/<eval_run_id>/scorecard.json
.staff/evals/<eval_run_id>/summary.md
```

## 分阶段 Checklist

### Phase 0：收敛 MVP 边界

- [ ] 确认 MVP 只做 headless CLI，不做完整 TUI。
- [ ] 确认主命令为 `staff exec --auto "<task>"`。
- [ ] 确认 `--auto` 下不等待人工审批：安全动作 allow，危险动作 deny。
- [ ] 确认 DeepSeek `deepseek-v4-pro` 是 Gate 和 eval 的验收 provider。
- [ ] 确认 `.staff/config.toml` 和 `.staff/ds-sk` 可用于本地探活；环境变量只作为覆盖项，不把 key 写入日志。
- [ ] 确认所有运行都必须生成 events、summary、artifact 和 failure category。

### Phase 1：工程基础与配置

- [ ] 引入 `clap`、`tokio`、`serde`、`toml`、`serde_json`、`tracing`。
- [ ] 用 `clap` 重写 CLI 参数解析。
- [ ] 用 `serde` 替换手写 `staff.toml` 解析。
- [ ] 建立统一 error 类型。
- [ ] 保留并验证基础命令：`staff help`、`staff tools`、`staff sandbox doctor`。
- [ ] 补齐 config 默认值、非法配置、CLI smoke 测试。

### Phase 2：Runtime、持久化与可观测骨架

- [ ] 为 `Thread`、`Turn`、`Item`、`ToolCall`、`ArtifactHandle`、`PermissionRequest` 补 serde。
- [ ] 实现 `.staff/threads/<thread_id>.jsonl` 追加写入和读取。
- [ ] 实现 `.staff/runs/<run_id>/events.jsonl`。
- [ ] 实现 `.staff/runs/<run_id>/summary.md`。
- [ ] 实现 `staff resume latest`。
- [ ] 实现 `staff runs list` 和 `staff runs show latest`。
- [ ] 所有 run 至少记录 `run.started`、`run.completed`、`run.failed`。
- [ ] 补齐 thread 恢复、损坏记录跳过、summary 可读测试。

### Phase 3：Provider 与 Agent Loop

- [ ] 定义 `ModelProvider` trait。
- [ ] 实现 DeepSeek OpenAI-compatible provider。
- [ ] 实现 `FakeProvider`，但仅用于离线开发和单元测试。
- [ ] 支持配置 `base_url`、`api_key_env`、`model`，默认 `https://api.deepseek.com` + `deepseek-v4-pro`。
- [ ] 支持从 `.staff/config.toml` 读取 provider 配置，并从 `.staff/ds-sk` 读取 key。
- [ ] 支持 `thinking = disabled` 的普通输出模式，避免小 token smoke 被 reasoning token 吃空。
- [ ] 实现非 streaming 的最小模型调用。
- [ ] 实现 plan-act-observe agent loop。
- [ ] 支持模型输出文本、tool call 请求和 final summary。
- [ ] 记录 `model.requested`、`model.completed`、`model.failed` 事件。
- [ ] 补齐无工具回答、单工具请求、工具失败恢复、模型配置错误测试。

### Phase 4：Tool Registry 与只读工具

- [ ] 实现 typed tool registry。
- [ ] 每个 tool 声明 name、input schema、permission action、output strategy、artifact strategy。
- [ ] 接入 `read`。
- [ ] 接入 `search`。
- [ ] 搜索和长文件输出写入 artifact，只回灌摘要。
- [ ] 记录 `tool.requested`、`tool.started`、`tool.completed`、`tool.failed`。
- [ ] 补齐 read/search 成功、失败、权限拒绝、artifact 创建测试。

### Phase 5：写入、Checkpoint 与 Diff

- [ ] 接入 `write`。
- [ ] 接入 `apply_patch`。
- [ ] 写文件或 patch 前创建 checkpoint。
- [ ] 写入后生成 diff artifact。
- [ ] 实现 `staff checkpoint restore <checkpoint_id>`。
- [ ] 记录 `file.checkpoint_created`、`file.diff_created`、`file.restored`。
- [ ] summary 中列出 changed files、checkpoint ids、diff artifact paths。
- [ ] 补齐工作区内写入、外部路径拒绝、restore 成功、diff 创建测试。
- [ ] 通过 Gate 1：`hello-world-write smoke`。

### Phase 6：权限自动模式与 Shell/Git 工具

- [ ] 实现 `--auto` 权限策略：safe allow，dangerous deny。
- [ ] 安全命令 allow：`rg`、`git status`、`git diff`、`cargo test`、`npm test`、`pnpm test`。
- [ ] 危险命令 deny：`rm -rf`、`sudo`、`curl | sh`、`git push`。
- [ ] 接入 `shell`。
- [ ] 接入 `git_status`。
- [ ] 接入 `git_diff`。
- [ ] 所有权限决策记录 `tool.permission_decided`。
- [ ] 补齐权限红队测试：敏感文件、工作区外写、危险命令、Plan 模式副作用。

### Phase 7：Context Pack 与 Artifact 管理

- [ ] 强化 repo map，跳过 `.git`、`target`、`node_modules`、`.staff`、cache/vendor。
- [ ] 加载 `AGENTS.md`。
- [ ] 加载 `.staff/skills/*/SKILL.md` 摘要。
- [ ] 加载历史 compaction 和 artifact summaries。
- [ ] 实现长会话 compaction。
- [ ] artifact 记录 kind、path、summary、sha256、bytes。
- [ ] 记录 `context.built`、`context.compacted`、`context.artifact_created`。
- [ ] 补齐长输出不污染主 context、compaction 保留关键信息测试。

### Phase 8：Eval Runner

- [ ] 实现 `staff eval run --suite smoke`。
- [ ] 实现 `staff eval compare <eval_a> <eval_b>`。
- [ ] 创建 `evals/fixtures/fix_small_bug`。
- [ ] 创建 `evals/fixtures/add_cli_option`。
- [ ] 创建 `evals/fixtures/update_config_parser`。
- [ ] 创建 `evals/suites/smoke.toml`。
- [ ] 输出 `.staff/evals/<eval_run_id>/events.jsonl`。
- [ ] 输出 `.staff/evals/<eval_run_id>/scorecard.json`。
- [ ] 输出 `.staff/evals/<eval_run_id>/summary.md`。
- [ ] 记录 `eval.started`、`eval.case_completed`、`eval.completed`。
- [ ] eval 必须真实调用 DeepSeek `deepseek-v4-pro`；fake-provider 结果只能作为本地开发参考。

### Phase 9：Dogfood、文档与发版门槛

- [ ] 用一个真实小仓库完成 dogfood：读代码、改文件、运行测试、总结证据。
- [ ] 完成 README quickstart。
- [ ] 提供 `staff.toml` 示例。
- [ ] 提供 `AGENTS.md` 示例。
- [ ] 确认 run summary 包含模型、上下文、工具时间线、文件变更、checkpoint、artifact、测试结果、失败归因。
- [ ] 工程回归全绿。
- [ ] 核心 golden eval 平均分不低于 8/11。
- [ ] 权限红队用例零副作用失败。

## 非 MVP 范围

- 完整 Ratatui TUI。
- 完整 OS 级强沙箱执行。
- 云端 VM、GitHub PR Agent。
- IDE 插件。
- 完整 MCP OAuth 和 marketplace。
- 企业治理、团队策略、远程任务调度。

这些内容在 headless MVP 跑通后进入 Beta 阶段。
