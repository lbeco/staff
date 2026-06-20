# Staff

**Staff** 是一个本地优先、终端优先的 Coding CLI Agent，使用 Rust 编写。它可以通过 TUI 交互运行，也可以通过 headless 命令自动执行任务；当前模型层使用 DeepSeek 的 OpenAI-compatible API，并围绕受控工具、权限、checkpoint、diff 和可观测记录构建闭环。

English: [README.md](README.md)

## 能做什么

- 通过 `staff` 启动终端 TUI。
- 通过 `staff exec --auto "<task>"` 运行非交互任务。
- 默认调用 DeepSeek `deepseek-v4-pro`。
- 写文件前创建 checkpoint，写文件后生成 diff artifact。
- 自动执行安全命令；遇到可人工判断但不在自动白名单内的命令时，在 TUI 中请求授权。
- 每次运行都会记录模型调用、工具调用、权限决策、artifact、summary 和失败归因。
- 内置真实 DeepSeek 回归评测：`staff eval run --suite tui_regression`。

开发阶段和里程碑验收标准已经移到 [docs/development-gates.md](docs/development-gates.md)。

## 安装

```bash
cargo install --path /Users/linbc/work/study/code/staff --force
```

本地开发常用命令：

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

## 配置 DeepSeek

在工作区根目录创建本地配置：

```bash
mkdir -p .staff
chmod 700 .staff
```

创建 `.staff/config.toml`：

```toml
model = "deepseek-v4-pro"
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"
api_key_file = ".staff/ds-sk"
max_output_tokens = 8192
```

你可以用环境变量配置 key：

```bash
export DEEPSEEK_API_KEY="..."
```

也可以把 key 放在 `.staff/ds-sk`：

```bash
printf '%s\n' 'your-deepseek-api-key' > .staff/ds-sk
chmod 600 .staff/ds-sk
```

`.staff/` 和 API key 文件不会提交到 Git。

## 使用

启动 TUI：

```bash
staff
```

带初始任务启动 TUI：

```bash
staff tui --prompt "读一下计算器代码，总结它做了什么"
```

运行 headless coding 任务：

```bash
staff exec --auto "Write a hello world program in this repo"
```

查看最近一次运行：

```bash
staff runs show latest
staff runs timeline latest
staff runs artifacts latest
staff runs failures
```

运行回归评测。该评测会真实调用 DeepSeek，不使用 fake provider 作为验收依据。

```bash
staff eval run --suite tui_regression
```

## TUI 快捷键

- `Enter`：空闲时提交；运行中排队。
- `Tab`：运行中排队；空闲时继续队列。
- `PageUp` / `PageDown`：滚动 Transcript。
- `Left` / `Right` / `Home` / `End`：编辑长输入。
- `Esc`：关闭 overlay、清空输入，或标记当前任务结束后停止队列。
- `Ctrl-C`：先清空输入，再按一次退出。
- 权限弹窗：`Y` / `Enter` 允许一次，`N` / `Esc` 拒绝。

Slash commands：

```text
/help
/runs
/timeline
/artifacts
/diff
/checkpoint
/clear
/quit
```

## 工具与权限

当前工具面包括：

- `write`：创建或覆盖工作区文件。
- `shell`：执行受控 shell 命令。
- `checkpoint`：创建或恢复文件快照。
- `git_status` / `git_diff`：查看仓库状态。
- `read`、`search`、`apply_patch`：CLI 已列出并纳入 MVP 计划的工具面。

默认安全策略：

- 工作区内安全写入自动允许。
- 常见测试命令自动允许，例如 `python3`、`pytest`、`cargo test`、`npm test`、`pnpm test`。
- 例如 `cat calculator.py` 这类可人工判断的命令会在 TUI 中请求授权。
- `rm`、`sudo`、shell 管道、重定向、父目录穿越、绝对路径等危险命令直接拒绝。
- headless `--auto` 不等待人工审批；非自动允许的命令会安全失败。

## 可观测记录

每次运行都会写入本地记录：

```text
.staff/runs/<run_id>/events.jsonl
.staff/runs/<run_id>/summary.md
.staff/artifacts/<artifact_id>.diff
.staff/artifacts/<artifact_id>.log
.staff/checkpoints/<checkpoint_id>/
```

API key 不会写入 run events 或 summaries。

## 目录结构

```text
src/main.rs        CLI 分发
src/runtime.rs     模型调用、工具执行、权限、checkpoint、run records
src/tui.rs         TUI 状态、渲染、输入、权限弹窗
src/eval.rs        真实 DeepSeek 回归评测 runner
evals/             易读的 eval case 定义
docs/              开发里程碑与补充文档
```

## 当前范围

Staff 目前是本地 Coding CLI 的 MVP，不是完整 IDE 插件、云端 VM Agent 或成熟多 Agent 编排系统。当前重点是把终端闭环做好：输入任务、调用模型、受控执行工具、生成 checkpoint/diff、跑真实 eval，并保留可追溯的运行历史。
