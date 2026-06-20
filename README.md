# staff

`staff` is a local headless coding CLI. Gate 1 supports:

```bash
staff exec --auto "Write a hello world program in this repo"
staff runs show latest
```

The current implementation is Rust and uses DeepSeek's OpenAI-compatible API with `deepseek-v4-pro`.

## Setup

Recommended workspace-local setup:

```bash
mkdir -p .staff
cp /Users/linbc/work/study/code/staff/ds-sk .staff/ds-sk
chmod 600 .staff/ds-sk
```

Create `.staff/config.toml`:

```toml
model = "deepseek-v4-pro"
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"
api_key_file = ".staff/ds-sk"
max_output_tokens = 8192
```

Environment variables still override the local config:

```bash
export DEEPSEEK_API_KEY="..."
export STAFF_BASE_URL="https://api.deepseek.com"
export STAFF_MODEL="deepseek-v4-pro"
export STAFF_MAX_OUTPUT_TOKENS="8192"
```

Install:

```bash
cargo install --path /Users/linbc/work/study/code/staff --force
```

## Gate 1 Outputs

Each run writes local observability artifacts:

```text
.staff/runs/<run_id>/events.jsonl
.staff/runs/<run_id>/summary.md
.staff/checkpoints/<checkpoint_id>/
.staff/artifacts/<artifact_id>.diff
```

No API key is written to run events or summaries.
