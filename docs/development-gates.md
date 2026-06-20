# Development Gates

This document keeps milestone and acceptance-gate details out of the user-facing README. The README should stay focused on installation, configuration, usage, safety, and observability.

## Gate 1: hello-world-write smoke

Minimum smoke command:

```bash
staff exec --auto "Write a hello world program in this repo"
staff runs show latest
```

Acceptance criteria:

- A file is created or modified in the workspace.
- The file contains a runnable hello world program.
- The run completes without manual approval.
- Writes stay inside the workspace.
- A checkpoint is created before the write.
- A diff artifact is created after the write.
- `.staff/runs/<run_id>/events.jsonl` exists.
- `.staff/runs/<run_id>/summary.md` exists.
- `staff runs show latest` shows the change, checkpoint, diff, and final summary.

Gate 1 must use a real DeepSeek `deepseek-v4-pro` call to produce the tool call.

## Gate 2: safe-shell verification smoke

Minimum smoke command:

```bash
staff exec --auto "Write a hello world program and run the project test command"
staff runs show latest
```

Acceptance criteria:

- Gate 1 still passes.
- Safe shell commands can run.
- Dangerous shell commands are denied.
- The run summary includes the test command, output summary, and failure category when relevant.
- Long command output is stored as an artifact rather than injected wholesale into the main context.

## Gate 3: eval smoke

Minimum smoke command:

```bash
staff eval run --suite tui_regression
```

Acceptance criteria:

- The suite is readable as markdown in `evals/tui_regression.md`.
- Eval cases call DeepSeek for real.
- Results include a `scorecard.json` and `summary.md`.
- Failed cases include enough run artifacts to diagnose the failure.
- The regression suite should pass before release-oriented pushes.

## Current Regression Command

```bash
cargo fmt --check
cargo test
cargo clippy -- -D warnings
staff eval run --suite tui_regression
```

## 中文说明

这个文件专门记录开发阶段 Gate 和验收标准，避免 README 变成研发计划文档。README 应该只服务新用户：它需要说明 Staff 是什么、怎么安装、怎么配置、怎么使用、权限策略是什么、运行记录在哪里。

### Gate 1：hello-world-write 冒烟测试

```bash
staff exec --auto "Write a hello world program in this repo"
staff runs show latest
```

验收标准：

- 工作区内确实创建或修改了文件。
- 文件内容是可运行的 hello world 程序。
- 全程不需要人工审批。
- 写入不越出 workspace。
- 写入前生成 checkpoint。
- 写入后生成 diff artifact。
- `.staff/runs/<run_id>/events.jsonl` 存在。
- `.staff/runs/<run_id>/summary.md` 存在。
- `staff runs show latest` 能看到变更、checkpoint、diff 和最终摘要。

### Gate 2：safe-shell verification 冒烟测试

```bash
staff exec --auto "Write a hello world program and run the project test command"
staff runs show latest
```

验收标准：

- Gate 1 仍然通过。
- 安全 shell 命令可执行。
- 危险 shell 命令会被拒绝。
- summary 中能看到测试命令、测试输出摘要和失败分类。
- 长输出落到 artifact，不污染主上下文。

### Gate 3：eval 冒烟测试

```bash
staff eval run --suite tui_regression
```

验收标准：

- 用例以 markdown 维护在 `evals/tui_regression.md`。
- 评测真实调用 DeepSeek。
- 输出 `scorecard.json` 和 `summary.md`。
- 失败用例有足够的运行记录用于排查。
- 面向发布或推送前，回归评测应保持通过。
