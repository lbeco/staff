# Staff

**Staff** is a local, terminal-first coding CLI agent written in Rust. It can run as an interactive TUI or as a headless command, call a DeepSeek OpenAI-compatible model, execute controlled tools, and record every run as local observability artifacts.

中文文档: [README.zh-CN.md](README.zh-CN.md)

## What It Does

- Runs in the terminal with `staff`.
- Supports headless automation with `staff exec --auto "<task>"`.
- Uses DeepSeek `deepseek-v4-pro` through an OpenAI-compatible HTTP API.
- Writes files with checkpoints and diff artifacts.
- Runs safe shell commands automatically and asks for approval in the TUI when a command is approvable but not auto-allowed.
- Records model calls, tool calls, permission decisions, artifacts, summaries, and failures under `.staff/`.
- Includes real DeepSeek regression evals in `staff eval run --suite tui_regression`.

Milestone and acceptance criteria live in [docs/development-gates.md](docs/development-gates.md).

## Install

```bash
cargo install --path /Users/linbc/work/study/code/staff --force
```

For local development:

```bash
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

## Configure DeepSeek

Create a workspace-local config:

```bash
mkdir -p .staff
chmod 700 .staff
```

Create `.staff/config.toml`:

```toml
model = "deepseek-v4-pro"
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"
api_key_file = ".staff/ds-sk"
max_output_tokens = 8192
```

Then either set an environment variable:

```bash
export DEEPSEEK_API_KEY="..."
```

or place the key in `.staff/ds-sk`:

```bash
printf '%s\n' 'your-deepseek-api-key' > .staff/ds-sk
chmod 600 .staff/ds-sk
```

API keys and `.staff/` runtime data are ignored by Git.

## Usage

Start the TUI:

```bash
staff
```

Start the TUI with an initial task:

```bash
staff tui --prompt "Read the calculator code and summarize what it does"
```

Run a headless coding task:

```bash
staff exec --auto "Write a hello world program in this repo"
```

Inspect the latest run:

```bash
staff runs show latest
staff runs timeline latest
staff runs artifacts latest
staff runs failures
```

Run the regression suite. These evals call DeepSeek for real; they do not use a fake provider as acceptance evidence.

```bash
staff eval run --suite tui_regression
```

## TUI Shortcuts

- `Enter`: submit when idle, queue while running.
- `Tab`: queue while running, resume queued tasks when idle.
- `PageUp` / `PageDown`: scroll the transcript.
- `Left` / `Right` / `Home` / `End`: edit long input.
- `Esc`: close overlays, clear input, or stop after the current run.
- `Ctrl-C`: clear input, then press again to quit.
- Permission prompt: `Y` / `Enter` allows once, `N` / `Esc` denies.

Slash commands:

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

## Tools And Permissions

Staff currently exposes these tool surfaces:

- `write`: create or overwrite workspace files.
- `shell`: run controlled shell commands.
- `checkpoint`: capture and restore file snapshots.
- `git_status` / `git_diff`: inspect repository state.
- `read`, `search`, and `apply_patch`: planned tool surfaces listed by the CLI and tracked in the MVP plan.

Safety defaults:

- Safe workspace writes are allowed.
- Common test commands such as `python3`, `pytest`, `cargo test`, `npm test`, and `pnpm test` are auto-allowed.
- Approvable commands such as `cat calculator.py` trigger a TUI permission prompt.
- Dangerous commands such as `rm`, `sudo`, shell pipelines, redirects, parent traversal, and absolute paths are denied.
- Headless `--auto` does not stop for manual approval; non-auto-allowed commands fail safely.

## Observability

Each run writes local artifacts:

```text
.staff/runs/<run_id>/events.jsonl
.staff/runs/<run_id>/summary.md
.staff/artifacts/<artifact_id>.diff
.staff/artifacts/<artifact_id>.log
.staff/checkpoints/<checkpoint_id>/
```

No API key is written to run events or summaries.

## Repository Layout

```text
src/main.rs        CLI dispatch
src/runtime.rs     model call, tool execution, permissions, checkpoints, run records
src/tui.rs         terminal UI state, rendering, input, permission overlay
src/eval.rs        real DeepSeek regression runner
evals/             readable eval case definitions
docs/              development milestones and supporting docs
```

## Current Scope

Staff is an MVP for a local coding CLI. It is not yet a full IDE extension, cloud VM agent, or multi-agent orchestration system. The current focus is a reliable terminal loop: prompt, model call, controlled tool execution, checkpoint, diff, eval, and observable run history.
