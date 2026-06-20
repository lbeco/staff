use crate::runtime::{
    self, PermissionDecision, PermissionRequest, ProviderConfig, Result, RunOutcome, RuntimeEvent,
};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde_json::Value;
use std::collections::VecDeque;
use std::io;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

type WorkerResult = std::result::Result<RunOutcome, String>;

pub(crate) fn run_tui(workspace: PathBuf, initial_prompt: Option<String>) -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let provider = runtime::load_provider_config(&workspace);
    let (event_tx, event_rx) = mpsc::channel::<RuntimeEvent>();
    let (permission_tx, permission_rx) = mpsc::channel::<PermissionRequest>();
    let (done_tx, done_rx) = mpsc::channel::<WorkerResult>();
    let mut app = TuiApp::new(workspace.clone(), provider);

    if let Some(prompt) = initial_prompt {
        start_prompt(
            &mut app,
            workspace.clone(),
            prompt,
            event_tx.clone(),
            permission_tx.clone(),
            done_tx.clone(),
        );
    }

    loop {
        drain_runtime_events(&mut app, &event_rx);
        drain_permission_requests(&mut app, &permission_rx);
        drain_worker_results(
            &mut app,
            &workspace,
            &event_tx,
            &permission_tx,
            &done_tx,
            &done_rx,
        );
        terminal.draw(|frame| render_app(frame, &mut app))?;

        if app.should_quit() {
            break;
        }

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => match app.handle_key(key) {
                    UiAction::None => {}
                    UiAction::Quit => break,
                    UiAction::StartRun(prompt) => start_prompt(
                        &mut app,
                        workspace.clone(),
                        prompt,
                        event_tx.clone(),
                        permission_tx.clone(),
                        done_tx.clone(),
                    ),
                },
                Event::Resize(_, _) => {}
                Event::Paste(text) => app.insert_text(&text.replace('\r', "\n")),
                _ => {}
            }
        }
    }

    terminal.show_cursor()?;
    Ok(())
}

fn start_prompt(
    app: &mut TuiApp,
    workspace: PathBuf,
    prompt: String,
    event_tx: Sender<RuntimeEvent>,
    permission_tx: Sender<PermissionRequest>,
    done_tx: Sender<WorkerResult>,
) {
    let conversation_context = app.conversation_context();
    app.mark_run_started(prompt.clone());
    thread::spawn(move || {
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            runtime::run_exec_auto_with_context(
                &workspace,
                &prompt,
                conversation_context,
                Some(event_tx),
                Some(permission_tx),
                false,
            )
        }))
        .map_or_else(
            |payload| Err(format!("worker panic: {}", panic_payload_message(payload))),
            |result| result.map_err(|err| err.0),
        );
        let _ = done_tx.send(result);
    });
}

fn panic_payload_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn drain_runtime_events(app: &mut TuiApp, event_rx: &Receiver<RuntimeEvent>) {
    while let Ok(event) = event_rx.try_recv() {
        app.apply_runtime_event(event);
    }
}

fn drain_permission_requests(app: &mut TuiApp, permission_rx: &Receiver<PermissionRequest>) {
    while let Ok(request) = permission_rx.try_recv() {
        app.set_pending_permission(request);
    }
}

fn drain_worker_results(
    app: &mut TuiApp,
    workspace: &Path,
    event_tx: &Sender<RuntimeEvent>,
    permission_tx: &Sender<PermissionRequest>,
    done_tx: &Sender<WorkerResult>,
    done_rx: &Receiver<WorkerResult>,
) {
    while let Ok(result) = done_rx.try_recv() {
        let next = app.mark_worker_finished(result);
        if let Some(prompt) = next {
            start_prompt(
                app,
                workspace.to_path_buf(),
                prompt,
                event_tx.clone(),
                permission_tx.clone(),
                done_tx.clone(),
            );
        }
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UiAction {
    None,
    Quit,
    StartRun(String),
}

#[derive(Debug, Clone)]
struct TranscriptCell {
    label: String,
    body: String,
    style: CellStyle,
}

#[derive(Debug, Clone, Copy)]
enum CellStyle {
    User,
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
struct Overlay {
    title: String,
    lines: Vec<String>,
    scroll: u16,
}

impl Overlay {
    fn new(title: impl Into<String>, lines: Vec<String>) -> Self {
        Self {
            title: title.into(),
            lines,
            scroll: 0,
        }
    }

    fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_add(5);
    }

    fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(5);
    }
}

#[derive(Debug)]
struct TuiApp {
    workspace: PathBuf,
    workspace_label: String,
    model: String,
    base_url: String,
    input: String,
    input_cursor: usize,
    input_view_start: usize,
    input_view_cols: usize,
    pending_permission: Option<PermissionRequest>,
    queue: VecDeque<String>,
    history: Vec<String>,
    history_cursor: Option<usize>,
    cells: Vec<TranscriptCell>,
    overlay: Option<Overlay>,
    running: bool,
    stop_after_current: bool,
    current_run_id: Option<String>,
    status: String,
    scroll_from_bottom: usize,
    transcript_page_rows: usize,
    quit_hint_until: Option<Instant>,
    changed_files: Vec<String>,
    checkpoints: Vec<String>,
    artifacts: Vec<String>,
    conversation: VecDeque<ConversationTurn>,
}

#[derive(Debug, Clone)]
struct ConversationTurn {
    user: String,
    assistant: String,
}

impl TuiApp {
    fn new(workspace: PathBuf, provider: ProviderConfig) -> Self {
        let workspace_label = workspace.display().to_string();
        Self {
            workspace,
            workspace_label,
            model: provider.model,
            base_url: provider.base_url,
            input: String::new(),
            input_cursor: 0,
            input_view_start: 0,
            input_view_cols: 24,
            pending_permission: None,
            queue: VecDeque::new(),
            history: Vec::new(),
            history_cursor: None,
            cells: vec![TranscriptCell {
                label: "system".to_string(),
                body: "Enter a coding task. Use /help for commands.".to_string(),
                style: CellStyle::Info,
            }],
            overlay: None,
            running: false,
            stop_after_current: false,
            current_run_id: None,
            status: "idle".to_string(),
            scroll_from_bottom: 0,
            transcript_page_rows: 8,
            quit_hint_until: None,
            changed_files: Vec::new(),
            checkpoints: Vec::new(),
            artifacts: Vec::new(),
            conversation: VecDeque::new(),
        }
    }

    fn should_quit(&self) -> bool {
        false
    }

    fn handle_key(&mut self, key: KeyEvent) -> UiAction {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return UiAction::None;
        }
        if self.pending_permission.is_some() {
            return self.handle_permission_key(key);
        }
        if self.overlay.is_some() {
            return self.handle_overlay_key(key);
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.handle_ctrl_c(),
            (KeyCode::Enter, _) => self.submit_input(),
            (KeyCode::Tab, _) => self.handle_tab(),
            (KeyCode::Esc, _) => {
                if self.input.is_empty() {
                    if self.running {
                        self.stop_after_current = true;
                        self.status = "will stop after current run".to_string();
                    }
                } else {
                    self.input.clear();
                    self.input_cursor = 0;
                    self.input_view_start = 0;
                    self.status = "input cleared".to_string();
                }
                UiAction::None
            }
            (KeyCode::Backspace, _) => {
                self.backspace_input();
                self.history_cursor = None;
                UiAction::None
            }
            (KeyCode::PageUp, _) => {
                self.scroll_transcript_up();
                UiAction::None
            }
            (KeyCode::PageDown, _) => {
                self.scroll_transcript_down();
                UiAction::None
            }
            (KeyCode::Left, _) => {
                self.move_input_left();
                UiAction::None
            }
            (KeyCode::Right, _) => {
                self.move_input_right();
                UiAction::None
            }
            (KeyCode::Home, _) => {
                self.move_input_home();
                UiAction::None
            }
            (KeyCode::End, _) => {
                self.move_input_end();
                UiAction::None
            }
            (KeyCode::Delete, _) => {
                self.delete_input_char();
                UiAction::None
            }
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.move_input_home();
                UiAction::None
            }
            (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.move_input_end();
                UiAction::None
            }
            (KeyCode::Up, _) => {
                self.history_up();
                UiAction::None
            }
            (KeyCode::Down, _) => {
                self.history_down();
                UiAction::None
            }
            (KeyCode::Char(ch), modifiers)
                if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT =>
            {
                self.insert_char(ch);
                self.history_cursor = None;
                UiAction::None
            }
            _ => UiAction::None,
        }
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) -> UiAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.overlay = None;
            }
            KeyCode::PageDown | KeyCode::Down => {
                if let Some(overlay) = &mut self.overlay {
                    overlay.scroll_down();
                }
            }
            KeyCode::PageUp | KeyCode::Up => {
                if let Some(overlay) = &mut self.overlay {
                    overlay.scroll_up();
                }
            }
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                self.overlay = None;
            }
            _ => {}
        }
        UiAction::None
    }

    fn handle_permission_key(&mut self, key: KeyEvent) -> UiAction {
        match key.code {
            KeyCode::Enter
            | KeyCode::Char('y')
            | KeyCode::Char('Y')
            | KeyCode::Char('a')
            | KeyCode::Char('A') => {
                self.decide_permission(PermissionDecision::Allow);
            }
            KeyCode::Esc
            | KeyCode::Char('n')
            | KeyCode::Char('N')
            | KeyCode::Char('d')
            | KeyCode::Char('D') => {
                self.decide_permission(PermissionDecision::Deny);
            }
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                self.decide_permission(PermissionDecision::Deny);
            }
            _ => {}
        }
        UiAction::None
    }

    fn decide_permission(&mut self, decision: PermissionDecision) {
        let Some(request) = self.pending_permission.take() else {
            return;
        };
        let decision_label = match decision {
            PermissionDecision::Allow => "allowed",
            PermissionDecision::Deny => "denied",
        };
        let _ = request.response_tx.send(decision);
        self.status = format!("permission {decision_label}; running");
        self.cells.push(TranscriptCell {
            label: "permission".to_string(),
            body: format!("{decision_label} {} {}", request.action, request.target),
            style: if decision_label == "allowed" {
                CellStyle::Success
            } else {
                CellStyle::Warning
            },
        });
    }

    fn handle_ctrl_c(&mut self) -> UiAction {
        if !self.input.is_empty() {
            self.input.clear();
            self.input_cursor = 0;
            self.input_view_start = 0;
            self.status = "input cleared".to_string();
            return UiAction::None;
        }
        let now = Instant::now();
        if self.quit_hint_until.is_some_and(|deadline| deadline > now) {
            return UiAction::Quit;
        }
        self.quit_hint_until = Some(now + Duration::from_secs(2));
        self.status = "press Ctrl-C again to quit".to_string();
        UiAction::None
    }

    fn handle_tab(&mut self) -> UiAction {
        if self.input.trim().is_empty() && !self.running {
            if let Some(prompt) = self.queue.pop_front() {
                self.stop_after_current = false;
                return UiAction::StartRun(prompt);
            }
            return UiAction::None;
        }
        self.submit_input()
    }

    fn submit_input(&mut self) -> UiAction {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return UiAction::None;
        }
        self.input.clear();
        self.input_cursor = 0;
        self.input_view_start = 0;
        self.history.push(text.clone());
        self.history_cursor = None;
        if text.starts_with('/') {
            return self.handle_slash(&text);
        }
        if self.running {
            self.queue.push_back(text);
            self.status = format!("{} queued", self.queue.len());
            UiAction::None
        } else {
            UiAction::StartRun(text)
        }
    }

    fn handle_slash(&mut self, command: &str) -> UiAction {
        match command {
            "/help" => {
                self.overlay = Some(Overlay::new(
                    "Help",
                    vec![
                        "Enter: submit when idle, queue while running".to_string(),
                        "Tab: queue while running, submit/resume queue when idle".to_string(),
                        "PageUp/PageDown: scroll transcript by page".to_string(),
                        "Left/Right/Home/End: move inside long input".to_string(),
                        "Permission prompt: Y/Enter allow, N/Esc deny".to_string(),
                        "Esc: close overlay, clear input, or stop after current run".to_string(),
                        "Ctrl-C: clear input, then press twice to quit".to_string(),
                        "/runs /timeline /artifacts /diff /checkpoint /clear /quit".to_string(),
                    ],
                ));
                UiAction::None
            }
            "/quit" => UiAction::Quit,
            "/clear" => {
                self.cells.clear();
                self.scroll_from_bottom = 0;
                self.status = "transcript cleared".to_string();
                UiAction::None
            }
            "/runs" => {
                self.open_result_overlay("Runs", runtime::format_runs(&self.workspace));
                UiAction::None
            }
            "/timeline" => {
                self.open_result_overlay(
                    "Timeline",
                    runtime::format_timeline(&self.workspace, "latest"),
                );
                UiAction::None
            }
            "/artifacts" => {
                self.open_result_overlay(
                    "Artifacts",
                    runtime::format_artifacts(&self.workspace, "latest"),
                );
                UiAction::None
            }
            "/diff" => {
                self.open_result_overlay("Diff", runtime::latest_diff_lines(&self.workspace));
                UiAction::None
            }
            "/checkpoint" => {
                self.open_result_overlay(
                    "Checkpoints",
                    runtime::format_checkpoints(&self.workspace),
                );
                UiAction::None
            }
            other => {
                self.cells.push(TranscriptCell {
                    label: "error".to_string(),
                    body: format!("unknown command: {other}"),
                    style: CellStyle::Error,
                });
                UiAction::None
            }
        }
    }

    fn open_result_overlay(&mut self, title: &str, result: Result<Vec<String>>) {
        let lines = result.unwrap_or_else(|err| vec![err.to_string()]);
        self.overlay = Some(Overlay::new(title, lines));
    }

    fn set_pending_permission(&mut self, request: PermissionRequest) {
        self.status = "awaiting permission".to_string();
        self.cells.push(TranscriptCell {
            label: "permission request".to_string(),
            body: format!("{} {}: {}", request.action, request.target, request.reason),
            style: CellStyle::Warning,
        });
        self.pending_permission = Some(request);
    }

    fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = self.history_cursor.map_or_else(
            || self.history.len().saturating_sub(1),
            |idx| idx.saturating_sub(1),
        );
        self.history_cursor = Some(next);
        if let Some(value) = self.history.get(next) {
            self.input = value.clone();
            self.move_input_end();
        }
    }

    fn history_down(&mut self) {
        let Some(idx) = self.history_cursor else {
            return;
        };
        let next = idx + 1;
        if next >= self.history.len() {
            self.history_cursor = None;
            self.input.clear();
            self.input_cursor = 0;
            self.input_view_start = 0;
        } else {
            self.history_cursor = Some(next);
            if let Some(value) = self.history.get(next) {
                self.input = value.clone();
                self.move_input_end();
            }
        }
    }

    fn insert_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.insert_char(ch);
        }
        self.history_cursor = None;
    }

    fn insert_char(&mut self, ch: char) {
        let byte_idx = byte_index_for_char(&self.input, self.input_cursor);
        self.input.insert(byte_idx, ch);
        self.input_cursor += 1;
    }

    fn backspace_input(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let start = byte_index_for_char(&self.input, self.input_cursor - 1);
        let end = byte_index_for_char(&self.input, self.input_cursor);
        self.input.replace_range(start..end, "");
        self.input_cursor -= 1;
        if self.input_cursor < self.input_view_start {
            self.input_view_start = self.input_cursor;
        }
    }

    fn delete_input_char(&mut self) {
        if self.input_cursor >= char_len(&self.input) {
            return;
        }
        let start = byte_index_for_char(&self.input, self.input_cursor);
        let end = byte_index_for_char(&self.input, self.input_cursor + 1);
        self.input.replace_range(start..end, "");
    }

    fn move_input_left(&mut self) {
        self.input_cursor = self.input_cursor.saturating_sub(1);
        if self.input_cursor < self.input_view_start {
            self.input_view_start = self.input_cursor;
        }
    }

    fn move_input_right(&mut self) {
        self.input_cursor = (self.input_cursor + 1).min(char_len(&self.input));
    }

    fn move_input_home(&mut self) {
        self.input_cursor = 0;
        self.input_view_start = 0;
    }

    fn move_input_end(&mut self) {
        self.input_cursor = char_len(&self.input);
    }

    fn scroll_transcript_up(&mut self) {
        let rows = self.transcript_page_rows.saturating_sub(1).max(3);
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(rows);
    }

    fn scroll_transcript_down(&mut self) {
        let rows = self.transcript_page_rows.saturating_sub(1).max(3);
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(rows);
    }

    fn ensure_input_cursor_visible(&mut self, cols: usize) {
        self.input_view_cols = cols.max(1);
        self.input_cursor = self.input_cursor.min(char_len(&self.input));
        self.input_view_start = self.input_view_start.min(self.input_cursor);
        if self.input_cursor < self.input_view_start {
            self.input_view_start = self.input_cursor;
        }
        let max_cursor_col = self.input_view_cols.saturating_sub(1);
        while display_width_between(&self.input, self.input_view_start, self.input_cursor)
            > max_cursor_col
            && self.input_view_start < self.input_cursor
        {
            self.input_view_start += 1;
        }
    }

    fn mark_run_started(&mut self, prompt: String) {
        self.running = true;
        self.stop_after_current = false;
        self.status = "running".to_string();
        self.scroll_from_bottom = 0;
        self.cells.push(TranscriptCell {
            label: "user".to_string(),
            body: prompt,
            style: CellStyle::User,
        });
    }

    fn mark_worker_finished(&mut self, result: WorkerResult) -> Option<String> {
        self.running = false;
        match result {
            Ok(outcome) => {
                self.current_run_id = Some(outcome.run_id);
                self.status = "completed".to_string();
                self.record_assistant_summary(outcome.final_summary);
            }
            Err(error) => {
                self.status = "failed".to_string();
                self.cells.push(TranscriptCell {
                    label: "worker".to_string(),
                    body: error,
                    style: CellStyle::Error,
                });
            }
        }
        if self.stop_after_current {
            self.stop_after_current = false;
            return None;
        }
        self.queue.pop_front()
    }

    fn conversation_context(&self) -> Option<String> {
        if self.conversation.is_empty() {
            return None;
        }
        let mut lines = Vec::new();
        for turn in self.conversation.iter().rev().take(4).rev() {
            lines.push(format!("User: {}", runtime::truncate(&turn.user, 220)));
            lines.push(format!(
                "Assistant: {}",
                runtime::truncate(&turn.assistant, 320)
            ));
        }
        Some(lines.join("\n"))
    }

    fn record_assistant_summary(&mut self, assistant: String) {
        let Some(user) = self
            .cells
            .iter()
            .rev()
            .find(|cell| cell.label == "user")
            .map(|cell| cell.body.clone())
        else {
            return;
        };
        self.conversation
            .push_back(ConversationTurn { user, assistant });
        while self.conversation.len() > 8 {
            self.conversation.pop_front();
        }
    }

    fn apply_runtime_event(&mut self, event: RuntimeEvent) {
        self.current_run_id = Some(event.run_id.clone());
        match event.event_type.as_str() {
            "run.started" => {
                self.status = "running".to_string();
                self.push_event_cell("run", &event, CellStyle::Info);
            }
            "context.built" => self.push_event_cell("context", &event, CellStyle::Info),
            "model.requested" => self.push_event_cell("model request", &event, CellStyle::Info),
            "model.completed" => {
                self.push_event_cell("model completed", &event, CellStyle::Success)
            }
            "model.failed" => self.push_event_cell("model failed", &event, CellStyle::Error),
            "tool.requested" => self.push_event_cell("tool request", &event, CellStyle::Info),
            "tool.permission_decided" => {
                self.push_event_cell("permission", &event, CellStyle::Warning)
            }
            "tool.permission_requested" => {
                self.push_event_cell("permission request", &event, CellStyle::Warning)
            }
            "tool.started" => self.push_event_cell("tool started", &event, CellStyle::Info),
            "tool.completed" => {
                if let Some(target) = str_field(&event.data, "target") {
                    push_unique(&mut self.changed_files, target);
                }
                self.push_event_cell("tool completed", &event, CellStyle::Success);
            }
            "file.checkpoint_created" => {
                if let Some(id) = str_field(&event.data, "checkpoint_id") {
                    push_unique(&mut self.checkpoints, id);
                }
                self.push_event_cell("checkpoint", &event, CellStyle::Success);
            }
            "file.diff_created" => {
                if let Some(path) = str_field(&event.data, "path") {
                    push_unique(&mut self.artifacts, path);
                }
                self.push_event_cell("diff", &event, CellStyle::Success);
            }
            "context.artifact_created" => {
                if let Some(artifact) = event.data.get("artifact") {
                    if let Some(path) = artifact.get("path").and_then(Value::as_str) {
                        push_unique(&mut self.artifacts, path.to_string());
                    }
                }
                self.push_event_cell("artifact", &event, CellStyle::Info);
            }
            "run.completed" => {
                self.status = "completed".to_string();
                self.push_event_cell("completed", &event, CellStyle::Success);
            }
            "run.failed" => {
                self.status = "failed".to_string();
                self.push_event_cell("failed", &event, CellStyle::Error);
            }
            _ => self.push_event_cell(&event.event_type, &event, CellStyle::Info),
        }
        self.scroll_from_bottom = 0;
    }

    fn push_event_cell(&mut self, label: &str, event: &RuntimeEvent, style: CellStyle) {
        self.cells.push(TranscriptCell {
            label: label.to_string(),
            body: summarize_event(event),
            style,
        });
    }
}

fn push_unique(items: &mut Vec<String>, item: String) {
    if !items.iter().any(|existing| existing == &item) {
        items.push(item);
    }
}

fn summarize_event(event: &RuntimeEvent) -> String {
    match event.event_type.as_str() {
        "run.started" => format!(
            "{} mode={}",
            str_field(&event.data, "prompt_summary").unwrap_or_default(),
            str_field(&event.data, "mode").unwrap_or_default()
        ),
        "context.built" => format!(
            "evidence={} constraints={}",
            number_field(&event.data, "evidence_count").unwrap_or(0),
            number_field(&event.data, "constraints_count").unwrap_or(0)
        ),
        "model.requested" => format!(
            "{} {}",
            str_field(&event.data, "provider").unwrap_or_default(),
            str_field(&event.data, "model").unwrap_or_default()
        ),
        "model.completed" => str_field(&event.data, "action_summary")
            .or_else(|| str_field(&event.data, "output_summary"))
            .unwrap_or_default(),
        "model.failed" => str_field(&event.data, "error").unwrap_or_default(),
        "tool.requested" => format!(
            "{} {}",
            str_field(&event.data, "name").unwrap_or_default(),
            str_field(&event.data, "input_summary").unwrap_or_default()
        ),
        "tool.permission_decided" => format!(
            "{} {} {}",
            str_field(&event.data, "decision").unwrap_or_default(),
            str_field(&event.data, "action").unwrap_or_default(),
            str_field(&event.data, "target").unwrap_or_default()
        ),
        "tool.permission_requested" => format!(
            "{} {}: {}",
            str_field(&event.data, "action").unwrap_or_default(),
            str_field(&event.data, "target").unwrap_or_default(),
            str_field(&event.data, "reason").unwrap_or_default()
        ),
        "tool.started" => format!(
            "{} {}",
            str_field(&event.data, "name").unwrap_or_default(),
            str_field(&event.data, "target").unwrap_or_default()
        ),
        "tool.completed" => format!(
            "{} checkpoint={} {}",
            str_field(&event.data, "target").unwrap_or_default(),
            str_field(&event.data, "checkpoint_id").unwrap_or_default(),
            str_field(&event.data, "output_summary").unwrap_or_default()
        ),
        "file.checkpoint_created" => format!(
            "{} {} existed={}",
            str_field(&event.data, "checkpoint_id").unwrap_or_default(),
            str_field(&event.data, "target").unwrap_or_default(),
            event
                .data
                .get("existed")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        ),
        "file.diff_created" => format!(
            "{} {}",
            str_field(&event.data, "target").unwrap_or_default(),
            str_field(&event.data, "path").unwrap_or_default()
        ),
        "context.artifact_created" => event
            .data
            .get("artifact")
            .map(|artifact| {
                format!(
                    "{} {}: {}",
                    artifact.get("kind").and_then(Value::as_str).unwrap_or(""),
                    artifact.get("path").and_then(Value::as_str).unwrap_or(""),
                    artifact
                        .get("summary")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                )
            })
            .unwrap_or_default(),
        "run.completed" => str_field(&event.data, "final_summary").unwrap_or_default(),
        "run.failed" => format!(
            "{} {}",
            str_field(&event.data, "failure_category").unwrap_or_default(),
            str_field(&event.data, "error").unwrap_or_default()
        ),
        _ => runtime::truncate(&event.data.to_string(), 240),
    }
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn number_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn render_app(frame: &mut Frame<'_>, app: &mut TuiApp) {
    let area = frame.area();
    let bottom_height = app.bottom_height(area.height);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(bottom_height),
        ])
        .split(area);

    render_header(frame, vertical[0], app);
    render_body(frame, vertical[1], app);
    render_bottom(frame, vertical[2], app);
    if app.overlay.is_some() {
        render_overlay(frame, centered_rect(86, 78, area), app);
    }
    if app.pending_permission.is_some() {
        render_permission_overlay(frame, centered_rect(78, 42, area), app);
    }
}

impl TuiApp {
    fn bottom_height(&self, terminal_height: u16) -> u16 {
        let queue_rows = if self.queue.is_empty() {
            0
        } else {
            self.queue.len().min(3) as u16 + 1
        };
        (4 + queue_rows).min(terminal_height.saturating_sub(6).max(4))
    }
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let run = app.current_run_id.as_deref().unwrap_or("-");
    let text = vec![
        Line::from(vec![
            Span::styled(
                "staff",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::raw(format!("model={}  ", app.model)),
            Span::raw(format!("status={}  ", app.status)),
            Span::raw(format!("run={}", run)),
        ]),
        Line::from(vec![
            Span::raw(format!("cwd={}  ", app.workspace_label)),
            Span::raw(format!("base_url={}", app.base_url)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn render_body(frame: &mut Frame<'_>, area: Rect, app: &mut TuiApp) {
    if area.width >= 110 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(60), Constraint::Length(34)])
            .split(area);
        render_transcript(frame, columns[0], app);
        render_side(frame, columns[1], app);
    } else {
        render_transcript(frame, area, app);
    }
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, app: &mut TuiApp) {
    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let viewport_rows = area.height.saturating_sub(2).max(1) as usize;
    app.transcript_page_rows = viewport_rows;
    let lines = transcript_lines(app, inner_width);
    let max_scroll = lines.len().saturating_sub(viewport_rows);
    app.scroll_from_bottom = app.scroll_from_bottom.min(max_scroll);
    let top = lines
        .len()
        .saturating_sub(viewport_rows)
        .saturating_sub(app.scroll_from_bottom);
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title("Transcript").borders(Borders::ALL))
            .scroll((top as u16, 0)),
        area,
    );
}

fn transcript_lines(app: &TuiApp, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for cell in &app.cells {
        let style = match cell.style {
            CellStyle::User => Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            CellStyle::Info => Style::default().fg(Color::Gray),
            CellStyle::Success => Style::default().fg(Color::Green),
            CellStyle::Warning => Style::default().fg(Color::Yellow),
            CellStyle::Error => Style::default().fg(Color::Red),
        };
        let prefix = format!("{} ", cell.label);
        let prefix_width = display_width(&prefix);
        let rest_prefix = if prefix_width >= width {
            String::new()
        } else {
            " ".repeat(prefix_width)
        };
        let first_width = width.saturating_sub(prefix_width).max(1);
        let rest_width = width.saturating_sub(display_width(&rest_prefix)).max(1);
        for (idx, chunk) in wrap_text(&cell.body, first_width, rest_width)
            .into_iter()
            .enumerate()
        {
            if idx == 0 {
                lines.push(Line::from(vec![
                    Span::styled(prefix.clone(), style),
                    Span::raw(chunk),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::raw(rest_prefix.clone()),
                    Span::raw(chunk),
                ]));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from("(empty)"));
    }
    lines
}

fn render_side(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let mut lines = Vec::new();
    lines.push(Line::from("Changed files").bold());
    extend_compact(&mut lines, &app.changed_files);
    lines.push(Line::from(""));
    lines.push(Line::from("Checkpoints").bold());
    extend_compact(&mut lines, &app.checkpoints);
    lines.push(Line::from(""));
    lines.push(Line::from("Artifacts").bold());
    extend_compact(&mut lines, &app.artifacts);
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title("Run").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn extend_compact(lines: &mut Vec<Line<'static>>, items: &[String]) {
    if items.is_empty() {
        lines.push(Line::from("  -"));
        return;
    }
    for item in items.iter().rev().take(4).rev() {
        lines.push(Line::from(format!("  {item}")));
    }
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

fn byte_index_for_char(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .map(|(idx, _)| idx)
        .nth(char_idx)
        .unwrap_or(text.len())
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn display_width_between(text: &str, start: usize, end: usize) -> usize {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(char_display_width)
        .sum()
}

fn char_display_width(ch: char) -> usize {
    match ch {
        '\n' => 2,
        '\t' => 4,
        _ => UnicodeWidthChar::width(ch).unwrap_or(0).max(1),
    }
}

fn visible_input_text(text: &str, start: usize, max_width: usize) -> String {
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars().skip(start) {
        let char_width = char_display_width(ch);
        if width + char_width > max_width && !out.is_empty() {
            break;
        }
        match ch {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("    "),
            _ => out.push(ch),
        }
        width += char_width;
        if width >= max_width {
            break;
        }
    }
    out
}

fn wrap_text(text: &str, first_width: usize, rest_width: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut is_first_chunk = true;
    for logical in text.split('\n') {
        let mut current = String::new();
        let mut current_width = 0usize;
        let mut limit = if is_first_chunk {
            first_width
        } else {
            rest_width
        }
        .max(1);
        if logical.is_empty() {
            chunks.push(String::new());
            is_first_chunk = false;
            continue;
        }
        for ch in logical.chars() {
            let width = char_display_width(ch);
            if current_width + width > limit && !current.is_empty() {
                chunks.push(current);
                current = String::new();
                current_width = 0;
                limit = rest_width.max(1);
            }
            current.push(ch);
            current_width += width;
        }
        chunks.push(current);
        is_first_chunk = false;
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

fn render_bottom(frame: &mut Frame<'_>, area: Rect, app: &mut TuiApp) {
    let mut lines = Vec::new();
    if !app.queue.is_empty() {
        lines.push(Line::from(format!("Queued follow-up inputs: {}", app.queue.len())).dim());
        for item in app.queue.iter().take(3) {
            lines.push(Line::from(format!("  > {}", runtime::truncate(item, 90))).dim());
        }
    }
    let composer_row = lines.len() as u16;
    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let prompt_width = 2usize;
    let input_cols = inner_width.saturating_sub(prompt_width).max(1);
    app.ensure_input_cursor_visible(input_cols);
    let visible_input = visible_input_text(&app.input, app.input_view_start, input_cols);
    let cursor_col = display_width_between(&app.input, app.input_view_start, app.input_cursor)
        .min(input_cols.saturating_sub(1));
    lines.push(Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::raw(visible_input),
    ]));
    let hint = if app.running {
        "Enter/Tab queue  Left/Right edit  Esc stop-after-current  Ctrl-C clear/quit"
    } else {
        "Enter submit  Left/Right edit  PageUp/PageDown scroll  /help  Ctrl-C quit"
    };
    lines.push(Line::from(hint).dim());
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().title("Input").borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
    let cursor_x = area
        .x
        .saturating_add(1)
        .saturating_add(prompt_width as u16)
        .saturating_add(cursor_col as u16);
    let cursor_y = area.y.saturating_add(1).saturating_add(composer_row);
    if cursor_x < area.x.saturating_add(area.width.saturating_sub(1))
        && cursor_y < area.y.saturating_add(area.height.saturating_sub(1))
    {
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));
    }
}

fn render_overlay(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let Some(overlay) = &app.overlay else {
        return;
    };
    let lines = overlay
        .lines
        .iter()
        .map(|line| Line::from(line.clone()))
        .collect::<Vec<_>>();
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(format!("{}  Esc/q closes", overlay.title))
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false })
            .scroll((overlay.scroll, 0)),
        area,
    );
}

fn render_permission_overlay(frame: &mut Frame<'_>, area: Rect, app: &TuiApp) {
    let Some(request) = &app.pending_permission else {
        return;
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("Run: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(request.run_id.clone()),
        ]),
        Line::from(vec![
            Span::styled("Action: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(request.action.clone()),
        ]),
        Line::from(vec![
            Span::styled("Target: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(request.target.clone()),
        ]),
        Line::from(vec![
            Span::styled("Reason: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(request.reason.clone()),
        ]),
        Line::from(""),
        Line::from("Y / Enter: allow this one command"),
        Line::from("N / Esc: deny"),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(format!("Permission Required  {}", request.id))
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn app_for_test() -> TuiApp {
        TuiApp::new(
            std::env::temp_dir().join("staff-tui-test"),
            ProviderConfig {
                base_url: "https://api.deepseek.com".to_string(),
                model: "deepseek-v4-pro".to_string(),
                api_key_env: "DEEPSEEK_API_KEY".to_string(),
                api_key_file: PathBuf::from(".staff/ds-sk"),
                max_output_tokens: 8192,
            },
        )
    }

    #[test]
    fn enter_starts_when_idle() {
        let mut app = app_for_test();
        app.input = "write hello".to_string();
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, UiAction::StartRun("write hello".to_string()));
    }

    #[test]
    fn tab_queues_when_running() {
        let mut app = app_for_test();
        app.running = true;
        app.input = "next task".to_string();
        app.input_cursor = char_len(&app.input);
        let action = app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(action, UiAction::None);
        assert_eq!(app.queue.len(), 1);
    }

    #[test]
    fn page_keys_scroll_transcript_by_page() {
        let mut app = app_for_test();
        app.transcript_page_rows = 10;
        let action = app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(action, UiAction::None);
        assert_eq!(app.scroll_from_bottom, 9);
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.scroll_from_bottom, 0);
    }

    #[test]
    fn input_cursor_moves_and_inserts_inside_text() {
        let mut app = app_for_test();
        app.insert_text("abcdef");
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE));
        assert_eq!(app.input, "abcdXef");
        assert_eq!(app.input_cursor, 5);
    }

    #[test]
    fn long_input_view_follows_cursor() {
        let mut app = app_for_test();
        app.insert_text("abcdefghijklmnopqrstuvwxyz");
        app.ensure_input_cursor_visible(8);
        assert!(app.input_view_start > 0);
        assert!(visible_input_text(&app.input, app.input_view_start, 8).contains('z'));
    }

    #[test]
    fn esc_closes_overlay() {
        let mut app = app_for_test();
        app.overlay = Some(Overlay::new("Diff", vec!["line".to_string()]));
        let action = app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, UiAction::None);
        assert!(app.overlay.is_none());
    }

    #[test]
    fn permission_prompt_allows_with_y() {
        let mut app = app_for_test();
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        app.set_pending_permission(PermissionRequest {
            id: "perm_1".to_string(),
            run_id: "run_1".to_string(),
            action: "shell".to_string(),
            target: "cat calculator.py".to_string(),
            reason: "not auto allowlisted".to_string(),
            response_tx,
        });
        let action = app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(action, UiAction::None);
        assert!(app.pending_permission.is_none());
        assert_eq!(
            response_rx.try_recv().expect("decision"),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn ctrl_c_double_quits() {
        let mut app = app_for_test();
        let first = app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        let second = app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(first, UiAction::None);
        assert_eq!(second, UiAction::Quit);
    }

    #[test]
    fn runtime_event_mapping_updates_cells() {
        let mut app = app_for_test();
        app.apply_runtime_event(RuntimeEvent {
            ts: "2026-06-20T00:00:00Z".to_string(),
            run_id: "run_1".to_string(),
            thread_id: "thr_1".to_string(),
            turn_id: "turn_1".to_string(),
            event_type: "tool.completed".to_string(),
            data: serde_json::json!({
                "target": "hello_world.py",
                "checkpoint_id": "chk_1",
                "output_summary": "created hello world"
            }),
        });
        assert_eq!(app.changed_files, vec!["hello_world.py".to_string()]);
        assert!(app.cells.iter().any(|cell| cell.label == "tool completed"));
    }

    #[test]
    fn conversation_context_keeps_previous_turn() {
        let mut app = app_for_test();
        app.mark_run_started("我想实现一个计算器,告诉我要怎么实现".to_string());
        app.record_assistant_summary("可以实现一个 Python 命令行计算器。".to_string());
        let context = app.conversation_context().expect("context");
        assert!(context.contains("计算器"));
        assert!(context.contains("Python"));
    }

    #[test]
    fn slash_runs_without_existing_data_does_not_panic() {
        let mut app = app_for_test();
        app.input = "/runs".to_string();
        let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, UiAction::None);
        assert!(app.overlay.is_some());
    }

    #[test]
    fn layout_smoke_narrow_and_wide() {
        for (width, height) in [(80, 24), (132, 32)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("test terminal");
            let mut app = app_for_test();
            terminal
                .draw(|frame| render_app(frame, &mut app))
                .expect("render should not fail");
        }
    }
}
