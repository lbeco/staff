use chrono::Utc;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(crate) const VERSION: &str = "0.1.0-gate1";
const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
const DEFAULT_MODEL: &str = "deepseek-v4-pro";
const DEFAULT_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 8192;
const TOOL_EVENT_OUTPUT_LIMIT: usize = 8192;
const TOOL_SUMMARY_OUTPUT_LIMIT: usize = 1200;

pub(crate) type Result<T> = std::result::Result<T, StaffError>;

#[derive(Debug, Clone)]
pub(crate) struct StaffError(pub(crate) String);

impl StaffError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for StaffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for StaffError {}

impl From<std::io::Error> for StaffError {
    fn from(value: std::io::Error) -> Self {
        Self::new(value.to_string())
    }
}

impl From<serde_json::Error> for StaffError {
    fn from(value: serde_json::Error) -> Self {
        Self::new(value.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RuntimeEvent {
    pub(crate) ts: String,
    pub(crate) run_id: String,
    pub(crate) thread_id: String,
    pub(crate) turn_id: String,
    pub(crate) event_type: String,
    pub(crate) data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermissionDecision {
    Allow,
    Deny,
}

#[derive(Debug)]
pub(crate) struct PermissionRequest {
    pub(crate) id: String,
    pub(crate) run_id: String,
    pub(crate) action: String,
    pub(crate) target: String,
    pub(crate) reason: String,
    pub(crate) response_tx: Sender<PermissionDecision>,
}

#[derive(Debug, Clone)]
pub(crate) struct RunOutcome {
    pub(crate) run_id: String,
    pub(crate) summary_path: PathBuf,
    pub(crate) final_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Artifact {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) summary: String,
    pub(crate) sha256: String,
    pub(crate) bytes: usize,
}

#[derive(Debug)]
struct RunRecorder {
    workspace: PathBuf,
    run_id: String,
    thread_id: String,
    turn_id: String,
    prompt: String,
    started: Instant,
    events_path: PathBuf,
    summary_path: PathBuf,
    model_calls: u32,
    tool_calls: u32,
    permission_allows: u32,
    permission_denies: u32,
    artifacts: Vec<Artifact>,
    checkpoints: Vec<String>,
    changed_files: Vec<String>,
    failure_category: Option<String>,
    event_tx: Option<Sender<RuntimeEvent>>,
    permission_tx: Option<Sender<PermissionRequest>>,
}

impl RunRecorder {
    fn new(
        workspace: PathBuf,
        prompt: String,
        event_tx: Option<Sender<RuntimeEvent>>,
        permission_tx: Option<Sender<PermissionRequest>>,
    ) -> Result<Self> {
        let run_id = new_id("run");
        let thread_id = new_id("thr");
        let turn_id = new_id("turn");
        let run_dir = workspace.join(".staff").join("runs").join(&run_id);
        fs::create_dir_all(&run_dir)?;
        let latest = workspace.join(".staff").join("runs").join("latest");
        if let Some(parent) = latest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&latest, format!("{run_id}\n"))?;
        Ok(Self {
            workspace,
            run_id,
            thread_id,
            turn_id,
            prompt,
            started: Instant::now(),
            events_path: run_dir.join("events.jsonl"),
            summary_path: run_dir.join("summary.md"),
            model_calls: 0,
            tool_calls: 0,
            permission_allows: 0,
            permission_denies: 0,
            artifacts: Vec::new(),
            checkpoints: Vec::new(),
            changed_files: Vec::new(),
            failure_category: None,
            event_tx,
            permission_tx,
        })
    }

    fn event(&self, event_type: &str, data: Value) -> Result<()> {
        let ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut event = json!({
            "ts": ts,
            "run_id": self.run_id,
            "thread_id": self.thread_id,
            "turn_id": self.turn_id,
            "type": event_type,
        });
        merge_json(&mut event, data);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_path)?;
        writeln!(file, "{}", serde_json::to_string(&event)?)?;
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(RuntimeEvent {
                ts,
                run_id: self.run_id.clone(),
                thread_id: self.thread_id.clone(),
                turn_id: self.turn_id.clone(),
                event_type: event_type.to_string(),
                data: event,
            });
        }
        Ok(())
    }

    fn artifact(&mut self, kind: &str, path: &Path, summary: &str) -> Result<Artifact> {
        let bytes = fs::read(path).unwrap_or_default();
        let artifact = Artifact {
            id: path
                .file_stem()
                .and_then(|item| item.to_str())
                .unwrap_or("artifact")
                .to_string(),
            kind: kind.to_string(),
            path: rel_string(path, &self.workspace)?,
            summary: summary.to_string(),
            sha256: hex_sha256(&bytes),
            bytes: bytes.len(),
        };
        self.artifacts.push(artifact.clone());
        self.event("context.artifact_created", json!({ "artifact": artifact }))?;
        Ok(artifact)
    }

    fn write_summary(&self, status: &str, final_summary: &str) -> Result<()> {
        let mut out = String::new();
        out.push_str(&format!("# Staff Run {}\n\n", self.run_id));
        out.push_str(&format!("- status: {status}\n"));
        out.push_str(&format!(
            "- duration_ms: {}\n",
            self.started.elapsed().as_millis()
        ));
        out.push_str(&format!("- model_calls: {}\n", self.model_calls));
        out.push_str(&format!("- tool_calls: {}\n", self.tool_calls));
        out.push_str(&format!(
            "- permission_allows: {}\n",
            self.permission_allows
        ));
        out.push_str(&format!(
            "- permission_denies: {}\n",
            self.permission_denies
        ));
        out.push_str(&format!(
            "- failure_category: {}\n\n",
            self.failure_category.as_deref().unwrap_or("none")
        ));
        out.push_str("## Prompt\n\n");
        out.push_str(&self.prompt);
        out.push_str("\n\n## Changed Files\n\n");
        if self.changed_files.is_empty() {
            out.push_str("- (none)\n");
        } else {
            for item in &self.changed_files {
                out.push_str(&format!("- {item}\n"));
            }
        }
        out.push_str("\n## Checkpoints\n\n");
        if self.checkpoints.is_empty() {
            out.push_str("- (none)\n");
        } else {
            for item in &self.checkpoints {
                out.push_str(&format!("- {item}\n"));
            }
        }
        out.push_str("\n## Artifacts\n\n");
        if self.artifacts.is_empty() {
            out.push_str("- (none)\n");
        } else {
            for item in &self.artifacts {
                out.push_str(&format!(
                    "- {} {}: {}\n",
                    item.kind, item.path, item.summary
                ));
            }
        }
        out.push_str("\n## Final Summary\n\n");
        out.push_str(final_summary);
        out.push('\n');
        fs::write(&self.summary_path, out)?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct ToolCall {
    tool: String,
    path: Option<String>,
    content: Option<String>,
    command: Option<String>,
    stdin: Option<String>,
    summary: String,
}

#[derive(Debug)]
enum ModelAction {
    Answer(String),
    Tool(ToolCall),
}

#[derive(Debug, Serialize)]
struct ContextPack {
    goal: String,
    constraints: Vec<String>,
    evidence: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderConfig {
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) api_key_env: String,
    pub(crate) api_key_file: PathBuf,
    pub(crate) max_output_tokens: u32,
}

impl ProviderConfig {
    fn load(workspace: &Path) -> Result<Self> {
        let mut config = Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            api_key_file: PathBuf::from(".staff/ds-sk"),
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        };

        for path in global_config_paths() {
            apply_config_file(&mut config, &path)?;
        }
        apply_config_file(&mut config, &workspace.join(".staff").join("config.toml"))?;
        Ok(config)
    }

    fn api_key(&self, workspace: &Path) -> Result<(String, String)> {
        if let Ok(value) = env::var(&self.api_key_env) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Ok((value, format!("env:{}", self.api_key_env)));
            }
        }
        if self.api_key_env != DEFAULT_API_KEY_ENV {
            if let Ok(value) = env::var(DEFAULT_API_KEY_ENV) {
                let value = value.trim().to_string();
                if !value.is_empty() {
                    return Ok((value, format!("env:{DEFAULT_API_KEY_ENV}")));
                }
            }
        }

        let configured = resolve_config_path(workspace, &self.api_key_file);
        if let Some(result) = read_key_file(workspace, &configured)? {
            return Ok(result);
        }

        let default_key_file = workspace.join(".staff").join("ds-sk");
        if configured != default_key_file {
            if let Some(result) = read_key_file(workspace, &default_key_file)? {
                return Ok(result);
            }
        }

        for global_key_file in global_key_paths() {
            if configured != global_key_file && default_key_file != global_key_file {
                if let Some(result) = read_key_file(workspace, &global_key_file)? {
                    return Ok(result);
                }
            }
        }

        let legacy_key_file = workspace.join("ds-sk");
        if let Some(result) = read_key_file(workspace, &legacy_key_file)? {
            return Ok(result);
        }

        let global_hint = global_key_paths()
            .into_iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(" or ");
        Err(StaffError::new(format!(
            "{} is required, or place a key at {} or {}",
            self.api_key_env,
            workspace.join(".staff").join("ds-sk").display(),
            global_hint
        )))
    }
}

fn apply_config_file(config: &mut ProviderConfig, path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(path)?;
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key {
            "base_url" => config.base_url = value.to_string(),
            "model" => config.model = value.to_string(),
            "api_key_env" => config.api_key_env = value.to_string(),
            "api_key_file" => config.api_key_file = PathBuf::from(value),
            "max_output_tokens" => {
                if let Ok(parsed) = value.parse::<u32>() {
                    config.max_output_tokens = parsed;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn global_config_paths() -> Vec<PathBuf> {
    global_staff_dirs()
        .into_iter()
        .map(|dir| dir.join("config.toml"))
        .collect()
}

fn global_key_paths() -> Vec<PathBuf> {
    global_staff_dirs()
        .into_iter()
        .map(|dir| dir.join("ds-sk"))
        .collect()
}

fn global_staff_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(staff_home) = env::var("STAFF_HOME") {
        let path = PathBuf::from(staff_home);
        if !path.as_os_str().is_empty() {
            dirs.push(path);
        }
    }
    if let Ok(home) = env::var("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join("staff"));
        dirs.push(home.join(".staff"));
    }
    dedupe_paths(dirs)
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if !out.iter().any(|existing| existing == &path) {
            out.push(path);
        }
    }
    out
}

pub(crate) fn load_provider_config(workspace: &Path) -> ProviderConfig {
    ProviderConfig::load(workspace).unwrap_or_else(|_| ProviderConfig {
        base_url: DEFAULT_BASE_URL.to_string(),
        model: DEFAULT_MODEL.to_string(),
        api_key_env: DEFAULT_API_KEY_ENV.to_string(),
        api_key_file: PathBuf::from(".staff/ds-sk"),
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
    })
}

pub(crate) fn run_exec_from_args(workspace: &Path, args: &[String]) -> Result<()> {
    let mut auto = false;
    let mut prompt = Vec::new();
    for arg in args {
        if arg == "--auto" {
            auto = true;
        } else {
            prompt.push(arg.as_str());
        }
    }
    if !auto {
        return Err(StaffError::new(
            "Gate 1 requires `staff exec --auto \"<task>\"`.",
        ));
    }
    let prompt = prompt.join(" ").trim().to_string();
    if prompt.is_empty() {
        return Err(StaffError::new("usage: staff exec --auto \"<task>\""));
    }
    run_exec_auto(workspace, &prompt, None, true).map(|_| ())
}

pub(crate) fn run_exec_auto(
    workspace: &Path,
    prompt: &str,
    event_tx: Option<Sender<RuntimeEvent>>,
    print_stdout: bool,
) -> Result<RunOutcome> {
    run_exec_auto_with_context(workspace, prompt, None, event_tx, None, print_stdout)
}

pub(crate) fn run_exec_auto_with_context(
    workspace: &Path,
    prompt: &str,
    conversation_context: Option<String>,
    event_tx: Option<Sender<RuntimeEvent>>,
    permission_tx: Option<Sender<PermissionRequest>>,
    print_stdout: bool,
) -> Result<RunOutcome> {
    let mut recorder = RunRecorder::new(
        workspace.to_path_buf(),
        prompt.to_string(),
        event_tx,
        permission_tx,
    )?;
    recorder.event(
        "run.started",
        json!({ "prompt_summary": truncate(prompt, 160), "mode": "auto" }),
    )?;
    let outcome = match run_exec_inner(
        workspace,
        prompt,
        conversation_context.as_deref(),
        &mut recorder,
    ) {
        Ok(outcome) => outcome,
        Err(err) => {
            recorder.failure_category = Some(classify_error(&err.0).to_string());
            let _ = recorder.event(
                "run.failed",
                json!({ "failure_category": recorder.failure_category, "error": err.0 }),
            );
            let _ = recorder.write_summary("failed", "Run failed. See events.jsonl for details.");
            return Err(StaffError::new(
                "run failed; see latest summary for details",
            ));
        }
    };
    if print_stdout {
        println!("{}", outcome.final_summary);
        println!("run_id: {}", outcome.run_id);
        println!("summary: {}", rel_string(&outcome.summary_path, workspace)?);
    }
    Ok(outcome)
}

fn run_exec_inner(
    workspace: &Path,
    prompt: &str,
    conversation_context: Option<&str>,
    recorder: &mut RunRecorder,
) -> Result<RunOutcome> {
    let context = build_context(workspace, prompt)?;
    recorder.event(
        "context.built",
        json!({
            "goal": prompt,
            "evidence_count": context.evidence.len(),
            "constraints_count": context.constraints.len(),
        }),
    )?;
    let action =
        request_deepseek_action(workspace, prompt, conversation_context, &context, recorder)?;
    let final_summary = match action {
        ModelAction::Answer(answer) => answer,
        ModelAction::Tool(tool_call) => {
            let result = execute_tool_call(workspace, recorder, &tool_call)?;
            tool_final_summary(prompt, &tool_call, &result)
        }
    };
    recorder.event("run.completed", json!({ "final_summary": final_summary }))?;
    recorder.write_summary("completed", &final_summary)?;
    Ok(RunOutcome {
        run_id: recorder.run_id.clone(),
        summary_path: recorder.summary_path.clone(),
        final_summary,
    })
}

struct ToolResult {
    path: String,
    checkpoint_id: Option<String>,
    diff_artifact: Option<Artifact>,
    output_summary: String,
}

fn tool_final_summary(prompt: &str, call: &ToolCall, result: &ToolResult) -> String {
    if response_language_hint(prompt) == "Chinese" {
        match (&result.checkpoint_id, &result.diff_artifact) {
            (Some(checkpoint_id), Some(diff_artifact)) => format!(
                "已完成：{}。修改文件 {}，checkpoint {}，diff artifact {}。",
                call.summary, result.path, checkpoint_id, diff_artifact.path
            ),
            _ => format!("已完成：{}。{}", call.summary, result.output_summary),
        }
    } else {
        match (&result.checkpoint_id, &result.diff_artifact) {
            (Some(checkpoint_id), Some(diff_artifact)) => format!(
                "Completed: {}. Changed {} with checkpoint {} and diff artifact {}.",
                call.summary, result.path, checkpoint_id, diff_artifact.path
            ),
            _ => format!("Completed: {}. {}", call.summary, result.output_summary),
        }
    }
}

fn request_deepseek_action(
    workspace: &Path,
    prompt: &str,
    conversation_context: Option<&str>,
    context: &ContextPack,
    recorder: &mut RunRecorder,
) -> Result<ModelAction> {
    let config = ProviderConfig::load(workspace)?;
    let (api_key, api_key_source) = config.api_key(workspace)?;
    let base_url = env::var("STAFF_BASE_URL").unwrap_or(config.base_url);
    let base_url = base_url.trim_end_matches('/').to_string();
    let model = env::var("STAFF_MODEL").unwrap_or(config.model);
    let max_output_tokens = env::var("STAFF_MAX_OUTPUT_TOKENS")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(config.max_output_tokens);
    let system = format!(
        "You are Staff, a coding CLI using the DeepSeek OpenAI-compatible provider with configured model `{model}`. If asked what model you are, answer that Staff is currently configured to use DeepSeek `{model}`. Always answer in the same natural language the user used, unless the user explicitly asks for another language. Return strict JSON only, with no Markdown. Choose exactly one response shape. Use conversation_context to resolve short follow-ups such as confirmations, '需要', '继续', or '帮我实现'. For questions, planning requests, design requests, or follow-ups that do not ask to edit files or execute commands, answer directly as {{\"kind\":\"answer\",\"answer\":\"short answer in the user's language\"}}. Modify files only when explicitly asked to create/change code, or when the user confirms implementation of the immediately previous design, using {{\"kind\":\"tool_call\",\"tool\":\"write\",\"path\":\"relative/path\",\"content\":\"file content\",\"summary\":\"short summary in the user's language\"}}. Execute safe workspace commands immediately when explicitly asked to run or test code; do not ask the user for another confirmation. Use {{\"kind\":\"tool_call\",\"tool\":\"shell\",\"command\":\"python3 calculator.py\",\"stdin\":\"2+3\\nq\\n\",\"summary\":\"short summary in the user's language\"}}. If the program is interactive, include enough stdin to complete the test and exit. The allowed tools are write and shell. For a hello world creation task, create a small runnable Python program named hello_world.py."
    );
    let user = json!({
        "task": prompt,
        "conversation_context": conversation_context.unwrap_or(""),
        "response_language": response_language_hint(prompt),
        "context": context,
        "requirements": [
            "Use a workspace-relative path.",
            "Do not use absolute paths or parent directory traversal.",
            "Return only JSON.",
            "Use the response_language for any answer or summary text."
        ],
    });
    let body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user.to_string() }
        ],
        "thinking": { "type": "disabled" },
        "stream": false,
        "max_tokens": max_output_tokens
    });

    recorder.model_calls += 1;
    recorder.event(
        "model.requested",
        json!({
            "provider": "deepseek",
            "model": model,
            "max_output_tokens": max_output_tokens,
            "base_url": base_url,
            "api_key_source": api_key_source
        }),
    )?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|err| StaffError::new(format!("failed to build HTTP client: {err}")))?;
    let response = client
        .post(format!("{base_url}/chat/completions"))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .map_err(|err| StaffError::new(format!("DeepSeek request failed: {err}")))?;
    let status = response.status();
    let raw = response
        .text()
        .map_err(|err| StaffError::new(format!("DeepSeek response read failed: {err}")))?;
    if !status.is_success() {
        recorder.event(
            "model.failed",
            json!({ "provider": "deepseek", "status": status.as_u16(), "error": truncate(&raw, 400) }),
        )?;
        return Err(StaffError::new(format!(
            "DeepSeek request failed with HTTP {}: {}",
            status.as_u16(),
            truncate(&raw, 240)
        )));
    }
    let payload: Value = serde_json::from_str(&raw)?;
    let content = payload
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or("");
    let action = parse_action_json(content);
    let action_summary = action
        .as_ref()
        .map(model_action_summary)
        .unwrap_or_else(|err| format!("unparsed model output: {}", err.0));
    recorder.event(
        "model.completed",
        json!({
            "provider": "deepseek",
            "model": payload.get("model").and_then(Value::as_str).unwrap_or(&model),
            "status": status.as_u16(),
            "output_summary": truncate(content, 240),
            "action_summary": action_summary,
            "usage": payload.get("usage").cloned().unwrap_or(Value::Null),
        }),
    )?;
    action
}

fn model_action_summary(action: &ModelAction) -> String {
    match action {
        ModelAction::Answer(answer) => answer.clone(),
        ModelAction::Tool(call) => {
            let target = call
                .path
                .as_deref()
                .or(call.command.as_deref())
                .unwrap_or("");
            format!("tool call: {} {}", call.tool, target)
        }
    }
}

fn parse_action_json(content: &str) -> Result<ModelAction> {
    let mut text = content.trim().to_string();
    if text.starts_with("```") {
        text = text.trim_matches('`').to_string();
        if text.trim_start().starts_with("json") {
            text = text.trim_start()[4..].trim().to_string();
        }
    }
    let start = text.find('{').ok_or_else(|| {
        StaffError::new(format!(
            "model did not return JSON: {}",
            truncate(content, 200)
        ))
    })?;
    let end = text.rfind('}').ok_or_else(|| {
        StaffError::new(format!(
            "model did not return JSON: {}",
            truncate(content, 200)
        ))
    })?;
    let value: Value = serde_json::from_str(&text[start..=end])?;
    if let Some(answer) = value.get("answer").and_then(Value::as_str) {
        let answer = answer.trim();
        if !answer.is_empty() && value.get("tool").is_none() {
            return Ok(ModelAction::Answer(answer.to_string()));
        }
    }
    let call: ToolCall = serde_json::from_value(value)?;
    match call.tool.as_str() {
        "write" => {
            if call.path.as_deref().unwrap_or("").trim().is_empty()
                || call.content.as_deref().unwrap_or("").trim().is_empty()
                || call.summary.trim().is_empty()
            {
                return Err(StaffError::new(
                    "model write tool call is missing required fields",
                ));
            }
        }
        "shell" => {
            if call.command.as_deref().unwrap_or("").trim().is_empty()
                || call.summary.trim().is_empty()
            {
                return Err(StaffError::new(
                    "model shell tool call is missing required fields",
                ));
            }
        }
        _ => return Err(StaffError::new(format!("unsupported tool: {}", call.tool))),
    }
    Ok(ModelAction::Tool(call))
}

fn response_language_hint(text: &str) -> &'static str {
    if text.chars().any(is_cjk) {
        "Chinese"
    } else {
        "same language as the user"
    }
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B820..=0x2CEAF
    )
}

fn execute_tool_call(
    workspace: &Path,
    recorder: &mut RunRecorder,
    call: &ToolCall,
) -> Result<ToolResult> {
    match call.tool.as_str() {
        "write" => execute_write_tool(workspace, recorder, call),
        "shell" => execute_shell_tool(workspace, recorder, call),
        _ => Err(StaffError::new(format!("unsupported tool: {}", call.tool))),
    }
}

fn execute_write_tool(
    workspace: &Path,
    recorder: &mut RunRecorder,
    call: &ToolCall,
) -> Result<ToolResult> {
    let path = call
        .path
        .as_deref()
        .ok_or_else(|| StaffError::new("write tool missing path"))?;
    let content = call
        .content
        .as_deref()
        .ok_or_else(|| StaffError::new("write tool missing content"))?;
    recorder.tool_calls += 1;
    recorder.event(
        "tool.requested",
        json!({ "name": call.tool, "input_summary": format!("{}: {}", path, call.summary) }),
    )?;
    let target = resolve_workspace_path(workspace, path)?;
    recorder.permission_allows += 1;
    recorder.event(
        "tool.permission_decided",
        json!({ "name": call.tool, "action": "write", "target": path, "decision": "allow" }),
    )?;
    recorder.event("tool.started", json!({ "name": call.tool, "target": path }))?;

    let before = fs::read_to_string(&target).unwrap_or_default();
    let checkpoint = create_checkpoint(workspace, &target, &before, recorder)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&target, content)?;
    let after = fs::read_to_string(&target)?;
    let diff_artifact = create_diff_artifact(workspace, &target, &before, &after, recorder)?;
    let rel = rel_string(&target, workspace)?;
    recorder.changed_files.push(rel.clone());
    let checkpoint_id = checkpoint.id.clone();
    recorder.event(
        "tool.completed",
        json!({
            "name": call.tool,
            "target": rel,
            "output_summary": call.summary,
            "checkpoint_id": checkpoint_id,
            "diff_artifact": diff_artifact,
        }),
    )?;
    Ok(ToolResult {
        path: rel,
        checkpoint_id: Some(checkpoint.id),
        diff_artifact: Some(diff_artifact),
        output_summary: call.summary.clone(),
    })
}

fn execute_shell_tool(
    workspace: &Path,
    recorder: &mut RunRecorder,
    call: &ToolCall,
) -> Result<ToolResult> {
    let command = call
        .command
        .as_deref()
        .ok_or_else(|| StaffError::new("shell tool missing command"))?;
    recorder.tool_calls += 1;
    recorder.event(
        "tool.requested",
        json!({ "name": call.tool, "input_summary": format!("{}: {}", command, call.summary) }),
    )?;
    let args = resolve_shell_permission(recorder, command)?;
    recorder.permission_allows += 1;
    recorder.event(
        "tool.permission_decided",
        json!({ "name": call.tool, "action": "shell", "target": command, "decision": "allow" }),
    )?;
    recorder.event(
        "tool.started",
        json!({ "name": call.tool, "target": command }),
    )?;

    let mut process = Command::new(&args[0]);
    process
        .args(&args[1..])
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = process.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Some(input) = call.stdin.as_deref() {
            stdin.write_all(input.as_bytes())?;
        }
    }
    let output = child.wait_with_output()?;
    complete_shell_tool(
        workspace,
        recorder,
        command,
        call,
        output.status.code(),
        &output.stdout,
        &output.stderr,
    )
}

fn resolve_shell_permission(recorder: &mut RunRecorder, command: &str) -> Result<Vec<String>> {
    match validate_safe_shell_command(command) {
        Ok(args) => Ok(args),
        Err(auto_err) => {
            let args = match parse_approvable_shell_command(command) {
                Ok(args) => args,
                Err(err) => {
                    recorder.permission_denies += 1;
                    recorder.event(
                        "tool.permission_decided",
                        json!({
                            "name": "shell",
                            "action": "shell",
                            "target": command,
                            "decision": "deny",
                            "reason": err.0,
                        }),
                    )?;
                    return Err(err);
                }
            };
            let decision = request_shell_permission(recorder, command, &auto_err.0)?;
            if decision == PermissionDecision::Allow {
                Ok(args)
            } else {
                recorder.permission_denies += 1;
                recorder.event(
                    "tool.permission_decided",
                    json!({
                        "name": "shell",
                        "action": "shell",
                        "target": command,
                        "decision": "deny",
                        "reason": "user denied permission request",
                    }),
                )?;
                Err(StaffError::new(format!(
                    "permission denied: user denied shell `{command}`"
                )))
            }
        }
    }
}

fn request_shell_permission(
    recorder: &mut RunRecorder,
    command: &str,
    reason: &str,
) -> Result<PermissionDecision> {
    let Some(permission_tx) = recorder.permission_tx.clone() else {
        recorder.permission_denies += 1;
        recorder.event(
            "tool.permission_decided",
            json!({
                "name": "shell",
                "action": "shell",
                "target": command,
                "decision": "deny",
                "reason": reason,
            }),
        )?;
        return Err(StaffError::new(reason.to_string()));
    };
    let request_id = new_id("perm");
    let (response_tx, response_rx) = mpsc::channel();
    recorder.event(
        "tool.permission_requested",
        json!({
            "request_id": request_id,
            "name": "shell",
            "action": "shell",
            "target": command,
            "reason": reason,
        }),
    )?;
    permission_tx
        .send(PermissionRequest {
            id: request_id,
            run_id: recorder.run_id.clone(),
            action: "shell".to_string(),
            target: command.to_string(),
            reason: reason.to_string(),
            response_tx,
        })
        .map_err(|_| StaffError::new("permission request failed: TUI channel closed"))?;
    response_rx
        .recv()
        .map_err(|_| StaffError::new("permission request failed: no decision received"))
}

fn complete_shell_tool(
    workspace: &Path,
    recorder: &mut RunRecorder,
    command: &str,
    call: &ToolCall,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<ToolResult> {
    let stdout = String::from_utf8_lossy(stdout).to_string();
    let stderr = String::from_utf8_lossy(stderr).to_string();
    let output_summary = shell_output_summary(code, &stdout, &stderr);
    let artifact = create_shell_artifact(workspace, command, code, &stdout, &stderr, recorder)?;
    recorder.event(
        "tool.completed",
        json!({
            "name": call.tool,
            "target": command,
            "output_summary": output_summary,
            "exit_code": code,
            "stdout": truncate(&stdout, TOOL_EVENT_OUTPUT_LIMIT),
            "stderr": truncate(&stderr, TOOL_EVENT_OUTPUT_LIMIT),
            "output_artifact": artifact,
        }),
    )?;
    Ok(ToolResult {
        path: command.to_string(),
        checkpoint_id: None,
        diff_artifact: None,
        output_summary,
    })
}

fn shell_output_summary(code: Option<i32>, stdout: &str, stderr: &str) -> String {
    let mut parts = vec![format!("exit_code={}", code.unwrap_or(-1))];
    let stdout = stdout.trim();
    if !stdout.is_empty() {
        parts.push(format!(
            "stdout: {}",
            truncate(stdout, TOOL_SUMMARY_OUTPUT_LIMIT)
        ));
    }
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        parts.push(format!(
            "stderr: {}",
            truncate(stderr, TOOL_SUMMARY_OUTPUT_LIMIT)
        ));
    }
    parts.join("; ")
}

fn validate_safe_shell_command(command: &str) -> Result<Vec<String>> {
    let args = parse_shell_command(command)?;
    validate_auto_shell_args(&args)?;
    Ok(args)
}

fn parse_shell_command(command: &str) -> Result<Vec<String>> {
    let command = command.trim();
    if command.is_empty() {
        return Err(StaffError::new("shell command is empty"));
    }
    for denied in [";", "&&", "||", "|", ">", "<", "`", "$(", "\n"] {
        if command.contains(denied) {
            return Err(StaffError::new(format!(
                "shell denied: command contains unsupported token `{denied}`"
            )));
        }
    }
    let args = command
        .split_whitespace()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if args.is_empty() {
        return Err(StaffError::new("shell command is empty"));
    }
    if args
        .iter()
        .skip(1)
        .any(|arg| arg.contains("..") || arg.starts_with('/'))
    {
        return Err(StaffError::new(
            "shell denied: absolute paths and parent traversal are not allowed",
        ));
    }
    Ok(args)
}

fn validate_auto_shell_args(args: &[String]) -> Result<()> {
    let Some(program) = args.first().map(String::as_str) else {
        return Err(StaffError::new("shell command is empty"));
    };
    let allowed = matches!(
        program,
        "python" | "python3" | "pytest" | "cargo" | "npm" | "pnpm" | "node"
    );
    if !allowed {
        return Err(StaffError::new(format!(
            "shell denied: `{program}` is not in the safe command allowlist"
        )));
    }
    if program == "cargo" && args.get(1).map(String::as_str) != Some("test") {
        return Err(StaffError::new(
            "shell denied: only `cargo test` is allowed",
        ));
    }
    if matches!(program, "npm" | "pnpm") && args.get(1).map(String::as_str) != Some("test") {
        return Err(StaffError::new(format!(
            "shell denied: only `{program} test` is allowed"
        )));
    }
    Ok(())
}

fn parse_approvable_shell_command(command: &str) -> Result<Vec<String>> {
    let args = parse_shell_command(command)?;
    let Some(program) = args.first().map(String::as_str) else {
        return Err(StaffError::new("shell command is empty"));
    };
    if matches!(
        program,
        "sudo"
            | "su"
            | "rm"
            | "mv"
            | "cp"
            | "chmod"
            | "chown"
            | "dd"
            | "mkfs"
            | "curl"
            | "wget"
            | "ssh"
            | "scp"
            | "rsync"
    ) {
        return Err(StaffError::new(format!(
            "shell denied: `{program}` is too risky for interactive approval"
        )));
    }
    Ok(args)
}

fn create_shell_artifact(
    workspace: &Path,
    command: &str,
    code: Option<i32>,
    stdout: &str,
    stderr: &str,
    recorder: &mut RunRecorder,
) -> Result<Artifact> {
    let artifact_id = new_id("shell");
    let artifact_path = workspace
        .join(".staff")
        .join("artifacts")
        .join(format!("{artifact_id}.log"));
    if let Some(parent) = artifact_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = format!(
        "command: {command}\nexit_code: {}\n\n--- stdout ---\n{stdout}\n\n--- stderr ---\n{stderr}\n",
        code.unwrap_or(-1)
    );
    fs::write(&artifact_path, content)?;
    recorder.artifact(
        "shell",
        &artifact_path,
        &format!("shell output for `{command}`"),
    )
}

#[derive(Serialize)]
struct CheckpointManifest {
    id: String,
    target: String,
    existed: bool,
    created_at: String,
}

fn create_checkpoint(
    workspace: &Path,
    target: &Path,
    before: &str,
    recorder: &mut RunRecorder,
) -> Result<CheckpointManifest> {
    let id = new_id("chk");
    let rel = rel_string(target, workspace)?;
    let checkpoint_dir = workspace.join(".staff").join("checkpoints").join(&id);
    let original = checkpoint_dir.join("files").join(&rel);
    if let Some(parent) = original.parent() {
        fs::create_dir_all(parent)?;
    }
    let existed = target.exists();
    fs::write(&original, before)?;
    let manifest = CheckpointManifest {
        id: id.clone(),
        target: rel.clone(),
        existed,
        created_at: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    };
    fs::write(
        checkpoint_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    recorder.checkpoints.push(id.clone());
    recorder.event(
        "file.checkpoint_created",
        json!({ "checkpoint_id": id, "target": rel, "existed": existed }),
    )?;
    Ok(manifest)
}

fn create_diff_artifact(
    workspace: &Path,
    target: &Path,
    before: &str,
    after: &str,
    recorder: &mut RunRecorder,
) -> Result<Artifact> {
    let artifact_id = new_id("diff");
    let artifact_path = workspace
        .join(".staff")
        .join("artifacts")
        .join(format!("{artifact_id}.diff"));
    if let Some(parent) = artifact_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let rel = rel_string(target, workspace)?;
    let diff = unified_diff(&rel, before, after);
    fs::write(&artifact_path, diff)?;
    recorder.event(
        "file.diff_created",
        json!({ "artifact_id": artifact_id, "target": rel, "path": rel_string(&artifact_path, workspace)? }),
    )?;
    recorder.artifact("diff", &artifact_path, &format!("diff for {rel}"))
}

fn unified_diff(path: &str, before: &str, after: &str) -> String {
    if before == after {
        return format!("--- a/{path}\n+++ b/{path}\n");
    }
    let before_lines = before.lines().collect::<Vec<_>>();
    let after_lines = after.lines().collect::<Vec<_>>();
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    out.push_str(&format!(
        "@@ -1,{} +1,{} @@\n",
        before_lines.len(),
        after_lines.len()
    ));
    for line in before_lines {
        out.push_str(&format!("-{line}\n"));
    }
    for line in after_lines {
        out.push_str(&format!("+{line}\n"));
    }
    out
}

fn resolve_workspace_path(workspace: &Path, path_text: &str) -> Result<PathBuf> {
    let path = Path::new(path_text);
    if path.is_absolute() {
        return Err(StaffError::new(format!(
            "write denied: absolute paths are not allowed: {path_text}"
        )));
    }
    if path
        .components()
        .any(|item| matches!(item, std::path::Component::ParentDir))
    {
        return Err(StaffError::new(format!(
            "write denied: parent traversal is not allowed: {path_text}"
        )));
    }
    let root = fs::canonicalize(workspace)?;
    let target = root.join(path);
    if let Some(parent) = target.parent() {
        if parent.exists() {
            let parent = fs::canonicalize(parent)?;
            if !parent.starts_with(&root) {
                return Err(StaffError::new(format!(
                    "write denied: target is outside workspace: {path_text}"
                )));
            }
        }
    }
    Ok(target)
}

fn build_context(workspace: &Path, prompt: &str) -> Result<ContextPack> {
    let mut evidence = Vec::new();
    visit_files(workspace, workspace, &mut evidence)?;
    let mut constraints = Vec::new();
    let agents = workspace.join("AGENTS.md");
    if let Ok(content) = fs::read_to_string(agents) {
        constraints.push(first_line(&content));
    }
    if constraints.is_empty() {
        constraints.push("No AGENTS.md found; follow local file conventions.".to_string());
    }
    Ok(ContextPack {
        goal: prompt.to_string(),
        constraints,
        evidence,
    })
}

fn visit_files(root: &Path, path: &Path, evidence: &mut Vec<String>) -> Result<()> {
    if evidence.len() >= 40 {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if should_skip(root, &path) {
            continue;
        }
        if path.is_dir() {
            visit_files(root, &path, evidence)?;
        } else if is_source_like(&path) {
            evidence.push(rel_string(&path, root)?);
            if evidence.len() >= 40 {
                break;
            }
        }
    }
    Ok(())
}

fn should_skip(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return true;
    };
    rel.components().any(|part| {
        matches!(
            part.as_os_str().to_str(),
            Some(".git" | ".staff" | "target" | "node_modules" | "__pycache__")
        )
    })
}

fn is_source_like(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|item| item.to_str()),
        Some("py" | "rs" | "md" | "toml" | "json" | "js" | "ts")
    )
}

pub(crate) fn run_runs(workspace: &Path, args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    let runs_dir = workspace.join(".staff").join("runs");
    match sub {
        "list" => {
            for line in format_runs(workspace)? {
                println!("{line}");
            }
            Ok(())
        }
        "show" => {
            let target = args.get(1).map(String::as_str).unwrap_or("latest");
            let run_id = resolve_run_id(&runs_dir, target)?;
            let summary = runs_dir.join(run_id).join("summary.md");
            print!("{}", fs::read_to_string(summary)?);
            Ok(())
        }
        "timeline" => {
            let target = args.get(1).map(String::as_str).unwrap_or("latest");
            for line in format_timeline(workspace, target)? {
                println!("{line}");
            }
            Ok(())
        }
        "artifacts" => {
            let target = args.get(1).map(String::as_str).unwrap_or("latest");
            for line in format_artifacts(workspace, target)? {
                println!("{line}");
            }
            Ok(())
        }
        "failures" => {
            if !runs_dir.exists() {
                return Ok(());
            }
            for entry in fs::read_dir(runs_dir)? {
                let entry = entry?;
                let events = entry.path().join("events.jsonl");
                if !events.exists() {
                    continue;
                }
                for line in BufReader::new(File::open(&events)?).lines() {
                    let item: Value = serde_json::from_str(&line?)?;
                    if item.get("type").and_then(Value::as_str) == Some("run.failed") {
                        println!(
                            "{}: {} {}",
                            entry.file_name().to_string_lossy(),
                            item.get("failure_category")
                                .and_then(Value::as_str)
                                .unwrap_or(""),
                            item.get("error").and_then(Value::as_str).unwrap_or("")
                        );
                    }
                }
            }
            Ok(())
        }
        _ => Err(StaffError::new(
            "usage: staff runs list|show|timeline|artifacts|failures [run_id|latest]",
        )),
    }
}

pub(crate) fn format_runs(workspace: &Path) -> Result<Vec<String>> {
    let runs_dir = workspace.join(".staff").join("runs");
    if !runs_dir.exists() {
        return Ok(vec!["No runs found.".to_string()]);
    }
    let mut entries = fs::read_dir(runs_dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    let mut lines = Vec::new();
    for entry in entries {
        if entry.path().is_dir() {
            lines.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    if lines.is_empty() {
        lines.push("No runs found.".to_string());
    }
    Ok(lines)
}

pub(crate) fn format_timeline(workspace: &Path, target: &str) -> Result<Vec<String>> {
    let runs_dir = workspace.join(".staff").join("runs");
    let run_id = resolve_run_id(&runs_dir, target)?;
    let events = File::open(runs_dir.join(run_id).join("events.jsonl"))?;
    let mut lines = Vec::new();
    for line in BufReader::new(events).lines() {
        let line = line?;
        let item: Value = serde_json::from_str(&line)?;
        lines.push(format!(
            "{} {} {} {}",
            item.get("ts").and_then(Value::as_str).unwrap_or(""),
            item.get("type").and_then(Value::as_str).unwrap_or(""),
            item.get("name").and_then(Value::as_str).unwrap_or(""),
            item.get("target").and_then(Value::as_str).unwrap_or("")
        ));
    }
    if lines.is_empty() {
        lines.push("(empty timeline)".to_string());
    }
    Ok(lines)
}

pub(crate) fn format_artifacts(workspace: &Path, target: &str) -> Result<Vec<String>> {
    let runs_dir = workspace.join(".staff").join("runs");
    let run_id = resolve_run_id(&runs_dir, target)?;
    let events = File::open(runs_dir.join(run_id).join("events.jsonl"))?;
    let mut lines = Vec::new();
    for line in BufReader::new(events).lines() {
        let item: Value = serde_json::from_str(&line?)?;
        if let Some(artifact) = item.get("artifact") {
            lines.push(format!(
                "{} {}: {}",
                artifact.get("kind").and_then(Value::as_str).unwrap_or(""),
                artifact.get("path").and_then(Value::as_str).unwrap_or(""),
                artifact
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ));
        }
    }
    if lines.is_empty() {
        lines.push("No artifacts found.".to_string());
    }
    Ok(lines)
}

pub(crate) fn format_checkpoints(workspace: &Path) -> Result<Vec<String>> {
    let checkpoints_dir = workspace.join(".staff").join("checkpoints");
    if !checkpoints_dir.exists() {
        return Ok(vec!["No checkpoints found.".to_string()]);
    }
    let mut entries = fs::read_dir(checkpoints_dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    let mut lines = Vec::new();
    for entry in entries {
        let manifest = entry.path().join("manifest.json");
        if manifest.exists() {
            let value: Value = serde_json::from_str(&fs::read_to_string(manifest)?)?;
            lines.push(format!(
                "{} {} existed={}",
                value.get("id").and_then(Value::as_str).unwrap_or(""),
                value.get("target").and_then(Value::as_str).unwrap_or(""),
                value
                    .get("existed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            ));
        }
    }
    if lines.is_empty() {
        lines.push("No checkpoints found.".to_string());
    }
    Ok(lines)
}

pub(crate) fn latest_diff_lines(workspace: &Path) -> Result<Vec<String>> {
    let runs_dir = workspace.join(".staff").join("runs");
    let run_id = resolve_run_id(&runs_dir, "latest")?;
    let events = File::open(runs_dir.join(run_id).join("events.jsonl"))?;
    let mut latest_path = None;
    for line in BufReader::new(events).lines() {
        let item: Value = serde_json::from_str(&line?)?;
        if item.get("type").and_then(Value::as_str) == Some("file.diff_created") {
            latest_path = item
                .get("path")
                .and_then(Value::as_str)
                .map(ToString::to_string);
        }
    }
    let Some(path) = latest_path else {
        return Ok(vec!["No diff artifact found.".to_string()]);
    };
    let content = fs::read_to_string(workspace.join(path))?;
    Ok(content.lines().map(ToString::to_string).collect())
}

fn resolve_run_id(runs_dir: &Path, target: &str) -> Result<String> {
    if target == "latest" {
        return Ok(fs::read_to_string(runs_dir.join("latest"))?
            .trim()
            .to_string());
    }
    Ok(target.to_string())
}

fn resolve_config_path(workspace: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    }
}

fn read_key_file(workspace: &Path, path: &Path) -> Result<Option<(String, String)>> {
    if !path.exists() {
        return Ok(None);
    }
    let value = fs::read_to_string(path)?
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    if value.is_empty() {
        return Ok(None);
    }
    let source = if let Ok(rel) = path.strip_prefix(workspace) {
        format!("file:{}", rel.to_string_lossy())
    } else {
        "file:<external>".to_string()
    };
    Ok(Some((value, source)))
}

pub(crate) fn run_tools() -> Result<()> {
    for (name, description) in [
        ("read", "Read workspace files"),
        ("search", "Search workspace files"),
        ("write", "Create or overwrite workspace files"),
        ("apply_patch", "Apply structured patches"),
        ("shell", "Run safe shell commands"),
        ("git_status", "Show git status"),
        ("git_diff", "Show git diff"),
        ("checkpoint", "Create or restore checkpoints"),
    ] {
        println!("{name}: {description}");
    }
    Ok(())
}

pub(crate) fn run_sandbox(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("doctor");
    if sub != "doctor" {
        return Err(StaffError::new("usage: staff sandbox doctor"));
    }
    println!("platform: {}", std::env::consts::OS);
    println!("supported: permission-gate");
    println!("mechanism: Gate 1 uses workspace path validation and dangerous-action deny rules.");
    Ok(())
}

pub(crate) fn run_checkpoint(workspace: &Path, args: &[String]) -> Result<()> {
    if args.len() < 2 || args[0] != "restore" {
        return Err(StaffError::new(
            "usage: staff checkpoint restore <checkpoint_id>",
        ));
    }
    let checkpoint_id = &args[1];
    let checkpoint_dir = workspace
        .join(".staff")
        .join("checkpoints")
        .join(checkpoint_id);
    let manifest_path = checkpoint_dir.join("manifest.json");
    let manifest: Value = serde_json::from_str(&fs::read_to_string(&manifest_path)?)?;
    let target_text = manifest
        .get("target")
        .and_then(Value::as_str)
        .ok_or_else(|| StaffError::new("checkpoint manifest missing target"))?;
    let target = resolve_workspace_path(workspace, target_text)?;
    let original = checkpoint_dir.join("files").join(target_text);
    if manifest
        .get("existed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(original, &target)?;
    } else if target.exists() {
        fs::remove_file(&target)?;
    }
    println!("restored {target_text} from {checkpoint_id}");
    Ok(())
}

pub(crate) fn print_help() {
    println!(
        "staff {VERSION}\n\nUSAGE:\n  staff\n  staff tui [--prompt \"<task>\"]\n  staff exec --auto \"<task>\"\n  staff eval run --suite tui_regression\n  staff runs list|show|timeline|artifacts|failures [run_id|latest]\n  staff checkpoint restore <checkpoint_id>\n  staff tools\n  staff sandbox doctor"
    );
}

fn merge_json(target: &mut Value, data: Value) {
    if let (Some(target), Some(data)) = (target.as_object_mut(), data.as_object()) {
        for (key, value) in data {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn rel_string(path: &Path, root: &Path) -> Result<String> {
    path.strip_prefix(root)
        .map(|item| item.to_string_lossy().to_string())
        .map_err(|_| StaffError::new(format!("path is outside workspace: {}", path.display())))
}

fn new_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}_{nanos:x}")
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn truncate(text: &str, limit: usize) -> String {
    let text = text.replace('\n', "\\n");
    if text.chars().count() <= limit {
        return text;
    }
    if limit == 0 {
        return String::new();
    }
    if limit <= 3 {
        return "...".chars().take(limit).collect();
    }
    let head = text.chars().take(limit - 3).collect::<String>();
    format!("{head}...")
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("(empty)")
        .chars()
        .take(160)
        .collect()
}

fn classify_error(message: &str) -> &'static str {
    let message = message.to_ascii_lowercase();
    if message.contains("deepseek") || message.contains("model") {
        "model_error"
    } else if message.contains("denied") || message.contains("permission") {
        "permission_denied"
    } else if message.contains("json") || message.contains("tool") {
        "tool_error"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_answer_action() {
        let action =
            parse_action_json(r#"{"kind":"answer","answer":"hello_world.py 是 Python。"}"#)
                .expect("answer parses");
        match action {
            ModelAction::Answer(answer) => assert_eq!(answer, "hello_world.py 是 Python。"),
            ModelAction::Tool(_) => panic!("expected answer"),
        }
    }

    #[test]
    fn parse_legacy_tool_action() {
        let action = parse_action_json(
            r#"{"tool":"write","path":"hello_world.py","content":"print(\"hi\")\n","summary":"create hello"}"#,
        )
        .expect("tool parses");
        match action {
            ModelAction::Tool(call) => {
                assert_eq!(call.tool, "write");
                assert_eq!(call.path.as_deref(), Some("hello_world.py"));
            }
            ModelAction::Answer(_) => panic!("expected tool"),
        }
    }

    #[test]
    fn parse_shell_tool_action() {
        let action = parse_action_json(
            r#"{"kind":"tool_call","tool":"shell","command":"python3 calculator.py","stdin":"2+3\nq\n","summary":"测试计算器"}"#,
        )
        .expect("shell tool parses");
        match action {
            ModelAction::Tool(call) => {
                assert_eq!(call.tool, "shell");
                assert_eq!(call.command.as_deref(), Some("python3 calculator.py"));
                assert_eq!(call.stdin.as_deref(), Some("2+3\nq\n"));
            }
            ModelAction::Answer(_) => panic!("expected tool"),
        }
    }

    #[test]
    fn validates_safe_shell_command() {
        let args = validate_safe_shell_command("python3 calculator.py").expect("safe command");
        assert_eq!(
            args,
            vec!["python3".to_string(), "calculator.py".to_string()]
        );
    }

    #[test]
    fn rejects_unsafe_shell_command() {
        assert!(validate_safe_shell_command("python3 calculator.py; rm -rf .").is_err());
        assert!(validate_safe_shell_command("sudo python3 calculator.py").is_err());
        assert!(validate_safe_shell_command("python3 ../calculator.py").is_err());
    }

    #[test]
    fn cat_shell_command_is_approvable_but_not_auto_allowed() {
        assert!(validate_safe_shell_command("cat calculator.py").is_err());
        let args = parse_approvable_shell_command("cat calculator.py").expect("approvable cat");
        assert_eq!(args, vec!["cat".to_string(), "calculator.py".to_string()]);
    }

    #[test]
    fn dangerous_shell_commands_are_not_approvable() {
        assert!(parse_approvable_shell_command("rm calculator.py").is_err());
        assert!(parse_approvable_shell_command("cat calculator.py; rm -rf .").is_err());
    }

    #[test]
    fn permission_request_can_allow_cat_execution() {
        let workspace = env::temp_dir().join(format!("staff-permission-{}", new_id("test")));
        fs::create_dir_all(&workspace).expect("workspace");
        fs::write(workspace.join("calculator.py"), "print('hi')\n").expect("fixture");
        let (permission_tx, permission_rx) = mpsc::channel::<PermissionRequest>();
        let approver = std::thread::spawn(move || {
            let request = permission_rx.recv().expect("permission request");
            assert_eq!(request.action, "shell");
            assert_eq!(request.target, "cat calculator.py");
            request
                .response_tx
                .send(PermissionDecision::Allow)
                .expect("send approval");
        });
        let mut recorder = RunRecorder::new(
            workspace.clone(),
            "read calculator".to_string(),
            None,
            Some(permission_tx),
        )
        .expect("recorder");
        let call = ToolCall {
            tool: "shell".to_string(),
            path: None,
            content: None,
            command: Some("cat calculator.py".to_string()),
            stdin: None,
            summary: "read calculator".to_string(),
        };
        let result = execute_shell_tool(&workspace, &mut recorder, &call).expect("shell result");
        approver.join().expect("approver thread");
        assert!(result.output_summary.contains("print('hi')"));
        fs::remove_dir_all(workspace).ok();
    }

    #[test]
    fn truncate_handles_multibyte_text() {
        let text = "我想要你帮我用Python实现一个计算器.你不要写代码,先告诉我需要实现哪些功能";
        let truncated = truncate(text, 24);
        assert!(truncated.ends_with("..."));
        assert!(truncated.is_char_boundary(truncated.len()));
        assert!(truncated.chars().count() <= 24);
    }

    #[test]
    fn detects_chinese_response_language() {
        assert_eq!(response_language_hint("帮我设计一个计算器"), "Chinese");
        assert_eq!(
            response_language_hint("Design a calculator"),
            "same language as the user"
        );
    }

    #[test]
    fn loads_max_output_tokens_from_config() {
        let workspace = std::env::temp_dir().join(format!("staff-config-test-{}", new_id("tmp")));
        std::fs::create_dir_all(workspace.join(".staff")).expect("mkdir");
        std::fs::write(
            workspace.join(".staff").join("config.toml"),
            "max_output_tokens = 1234\n",
        )
        .expect("write config");
        let config = ProviderConfig::load(&workspace).expect("load config");
        assert_eq!(config.max_output_tokens, 1234);
        let _ = std::fs::remove_dir_all(workspace);
    }
}
