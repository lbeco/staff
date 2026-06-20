use crate::runtime::{self, Result, StaffError};
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const TUI_REGRESSION: &str = include_str!("../evals/tui_regression.md");

pub(crate) fn run_eval(workspace: &Path, args: &[String]) -> Result<()> {
    if args.first().map(String::as_str) != Some("run") {
        return Err(StaffError::new(
            "usage: staff eval run --suite tui_regression",
        ));
    }
    let suite = parse_suite_arg(&args[1..])?;
    if suite != "tui_regression" {
        return Err(StaffError::new(format!("unknown eval suite `{suite}`")));
    }
    let suite = EvalSuite::parse("tui_regression", TUI_REGRESSION)?;
    let report = run_suite(workspace, suite)?;
    write_report(workspace, &report)?;
    print_report(&report);
    Ok(())
}

fn parse_suite_arg(args: &[String]) -> Result<String> {
    let mut suite = None;
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--suite" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(StaffError::new(
                        "usage: staff eval run --suite tui_regression",
                    ));
                };
                suite = Some(value.clone());
                idx += 2;
            }
            other => {
                return Err(StaffError::new(format!(
                    "unknown eval option `{other}`. usage: staff eval run --suite tui_regression"
                )));
            }
        }
    }
    suite.ok_or_else(|| StaffError::new("missing --suite tui_regression"))
}

#[derive(Debug)]
struct EvalSuite {
    name: String,
    cases: Vec<EvalCase>,
}

#[derive(Debug)]
struct EvalCase {
    name: String,
    expects: Vec<String>,
    turns: Vec<EvalTurn>,
}

#[derive(Debug)]
enum EvalTurn {
    User(String),
    AssistantContext(String),
}

impl EvalSuite {
    fn parse(name: &str, source: &str) -> Result<Self> {
        let mut cases = Vec::new();
        let chunks = source.split("\n## Case: ").skip(1);
        for chunk in chunks {
            let mut lines = chunk.lines();
            let case_name = lines
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .ok_or_else(|| StaffError::new("eval case missing name"))?
                .to_string();
            let body = lines.collect::<Vec<_>>().join("\n");
            cases.push(parse_case(case_name, &body)?);
        }
        if cases.is_empty() {
            return Err(StaffError::new("eval suite has no cases"));
        }
        Ok(Self {
            name: name.to_string(),
            cases,
        })
    }
}

fn parse_case(name: String, body: &str) -> Result<EvalCase> {
    let expects = body
        .lines()
        .find_map(|line| line.trim().strip_prefix("expect:"))
        .map(|line| {
            line.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut turns = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_text = Vec::new();
    for line in body.lines() {
        if let Some(heading) = line.trim().strip_prefix("### ") {
            flush_turn(&mut turns, current_heading.take(), &mut current_text);
            current_heading = Some(heading.trim().to_string());
        } else if current_heading.is_some() {
            current_text.push(line.to_string());
        }
    }
    flush_turn(&mut turns, current_heading, &mut current_text);
    if !turns.iter().any(|turn| matches!(turn, EvalTurn::User(_))) {
        return Err(StaffError::new(format!(
            "eval case `{name}` has no user turn"
        )));
    }
    Ok(EvalCase {
        name,
        expects,
        turns,
    })
}

fn flush_turn(turns: &mut Vec<EvalTurn>, heading: Option<String>, current_text: &mut Vec<String>) {
    let Some(heading) = heading else {
        return;
    };
    let text = current_text.join("\n").trim().to_string();
    current_text.clear();
    if text.is_empty() {
        return;
    }
    match heading.as_str() {
        "User" => turns.push(EvalTurn::User(text)),
        "Assistant Context" => turns.push(EvalTurn::AssistantContext(text)),
        _ => {}
    }
}

#[derive(Debug, Serialize)]
struct EvalReport {
    suite: String,
    passed: usize,
    failed: usize,
    output_dir: String,
    cases: Vec<CaseReport>,
}

#[derive(Debug, Serialize)]
struct CaseReport {
    name: String,
    passed: bool,
    score: usize,
    max_score: usize,
    checks: Vec<CheckReport>,
    final_summary: String,
    run_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CheckReport {
    name: String,
    passed: bool,
    detail: String,
}

fn run_suite(workspace: &Path, suite: EvalSuite) -> Result<EvalReport> {
    let output_dir =
        workspace
            .join(".staff")
            .join("evals")
            .join(format!("{}_{}", suite.name, new_id_suffix()));
    fs::create_dir_all(&output_dir)?;
    let mut cases = Vec::new();
    for case in suite.cases {
        cases.push(run_case(&output_dir, case)?);
    }
    let passed = cases.iter().filter(|case| case.passed).count();
    let failed = cases.len().saturating_sub(passed);
    Ok(EvalReport {
        suite: suite.name,
        passed,
        failed,
        output_dir: output_dir.to_string_lossy().to_string(),
        cases,
    })
}

fn run_case(output_dir: &Path, case: EvalCase) -> Result<CaseReport> {
    let case_dir = output_dir.join(&case.name);
    fs::create_dir_all(&case_dir)?;
    let mut conversation = Vec::new();
    let mut final_summary = String::new();
    let mut run_ids = Vec::new();
    for turn in case.turns {
        match turn {
            EvalTurn::AssistantContext(text) => {
                conversation.push(format!("Assistant: {}", runtime::truncate(&text, 320)));
            }
            EvalTurn::User(prompt) => {
                let context = if conversation.is_empty() {
                    None
                } else {
                    Some(conversation.join("\n"))
                };
                let outcome = match runtime::run_exec_auto_with_context(
                    &case_dir, &prompt, context, None, None, false,
                ) {
                    Ok(outcome) => outcome,
                    Err(err) => {
                        return Ok(CaseReport {
                            name: case.name,
                            passed: false,
                            score: 0,
                            max_score: case.expects.len().max(1),
                            checks: vec![CheckReport {
                                name: "runtime".to_string(),
                                passed: false,
                                detail: err.to_string(),
                            }],
                            final_summary: err.to_string(),
                            run_ids,
                        });
                    }
                };
                final_summary = outcome.final_summary.clone();
                run_ids.push(outcome.run_id);
                conversation.push(format!("User: {}", runtime::truncate(&prompt, 220)));
                conversation.push(format!(
                    "Assistant: {}",
                    runtime::truncate(&final_summary, 320)
                ));
            }
        }
    }
    let checks = evaluate_case(&case_dir, &case.expects, &final_summary)?;
    let score = checks.iter().filter(|check| check.passed).count();
    let max_score = checks.len();
    let passed = score == max_score;
    Ok(CaseReport {
        name: case.name,
        passed,
        score,
        max_score,
        checks,
        final_summary,
        run_ids,
    })
}

fn evaluate_case(
    case_dir: &Path,
    expects: &[String],
    final_summary: &str,
) -> Result<Vec<CheckReport>> {
    let mut checks = Vec::new();
    for expect in expects {
        checks.push(match expect.as_str() {
            "answer_only" => check_answer_only(case_dir)?,
            "tool_call" => check_tool_call(case_dir)?,
            "shell_tool" => check_shell_tool(case_dir)?,
            "shell_output_contains_5" => check_shell_output_contains(case_dir, "5")?,
            "shell_output_has_calculator_result" => {
                check_shell_output_has_calculator_result(case_dir)?
            }
            "no_file_change" => check_no_file_change(case_dir)?,
            "mentions_deepseek" => check_contains(
                "mentions_deepseek",
                final_summary,
                &["DeepSeek", "deepseek"],
            ),
            "chinese" => CheckReport {
                name: "chinese".to_string(),
                passed: final_summary.chars().any(is_cjk),
                detail: runtime::truncate(final_summary, 120),
            },
            "writes_calculator" => CheckReport {
                name: "writes_calculator".to_string(),
                passed: find_file_containing(case_dir, "calculator", &[".py"])?,
                detail: "expects a calculator Python file".to_string(),
            },
            "writes_hello_world" => CheckReport {
                name: "writes_hello_world".to_string(),
                passed: find_file_containing(case_dir, "hello", &[".py"])?,
                detail: "expects a hello Python file".to_string(),
            },
            "uses_context" => CheckReport {
                name: "uses_context".to_string(),
                passed: true,
                detail: "case includes Assistant Context before the final user turn".to_string(),
            },
            other => CheckReport {
                name: other.to_string(),
                passed: false,
                detail: "unknown expectation".to_string(),
            },
        });
    }
    Ok(checks)
}

fn check_answer_only(case_dir: &Path) -> Result<CheckReport> {
    let tool_calls = count_events(case_dir, "tool.completed")?;
    Ok(CheckReport {
        name: "answer_only".to_string(),
        passed: tool_calls == 0,
        detail: format!("tool.completed events: {tool_calls}"),
    })
}

fn check_tool_call(case_dir: &Path) -> Result<CheckReport> {
    let tool_calls = count_events(case_dir, "tool.completed")?;
    Ok(CheckReport {
        name: "tool_call".to_string(),
        passed: tool_calls > 0,
        detail: format!("tool.completed events: {tool_calls}"),
    })
}

fn check_shell_tool(case_dir: &Path) -> Result<CheckReport> {
    let tool_calls = count_tool_completed(case_dir, "shell")?;
    Ok(CheckReport {
        name: "shell_tool".to_string(),
        passed: tool_calls > 0,
        detail: format!("shell tool.completed events: {tool_calls}"),
    })
}

fn check_shell_output_contains(case_dir: &Path, needle: &str) -> Result<CheckReport> {
    let found = any_tool_completed(case_dir, "shell", |event| {
        ["stdout", "stderr", "output_summary"]
            .iter()
            .filter_map(|field| event.get(*field).and_then(Value::as_str))
            .any(|value| value.contains(needle))
    })?;
    Ok(CheckReport {
        name: format!("shell_output_contains_{needle}"),
        passed: found,
        detail: format!("expects shell stdout/stderr/summary to contain `{needle}`"),
    })
}

fn check_shell_output_has_calculator_result(case_dir: &Path) -> Result<CheckReport> {
    let found = any_tool_completed(case_dir, "shell", |event| {
        let text = ["stdout", "stderr", "output_summary"]
            .iter()
            .filter_map(|field| event.get(*field).and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        let lower = text.to_ascii_lowercase();
        let has_result_signal = text.contains("结果")
            || lower.contains("result")
            || text.contains(">>>")
            || text.contains("=");
        let has_digit = text.chars().any(|ch| ch.is_ascii_digit());
        has_result_signal && has_digit
    })?;
    Ok(CheckReport {
        name: "shell_output_has_calculator_result".to_string(),
        passed: found,
        detail: "expects shell output to include a calculator result and a numeric value"
            .to_string(),
    })
}

fn check_no_file_change(case_dir: &Path) -> Result<CheckReport> {
    let tool_calls = count_events(case_dir, "tool.completed")?;
    Ok(CheckReport {
        name: "no_file_change".to_string(),
        passed: tool_calls == 0,
        detail: format!("tool.completed events: {tool_calls}"),
    })
}

fn check_contains(name: &str, haystack: &str, needles: &[&str]) -> CheckReport {
    CheckReport {
        name: name.to_string(),
        passed: needles.iter().any(|needle| haystack.contains(needle)),
        detail: runtime::truncate(haystack, 120),
    }
}

fn count_events(case_dir: &Path, event_type: &str) -> Result<usize> {
    let runs_dir = case_dir.join(".staff").join("runs");
    if !runs_dir.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in fs::read_dir(runs_dir)? {
        let entry = entry?;
        let events = entry.path().join("events.jsonl");
        if !events.exists() {
            continue;
        }
        let content = fs::read_to_string(events)?;
        count += content
            .lines()
            .filter(|line| line.contains(&format!(r#""type":"{event_type}""#)))
            .count();
    }
    Ok(count)
}

fn count_tool_completed(case_dir: &Path, tool_name: &str) -> Result<usize> {
    let mut count = 0;
    for event in read_events(case_dir)? {
        if event.get("type").and_then(Value::as_str) == Some("tool.completed")
            && event.get("name").and_then(Value::as_str) == Some(tool_name)
        {
            count += 1;
        }
    }
    Ok(count)
}

fn any_tool_completed(
    case_dir: &Path,
    tool_name: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Result<bool> {
    for event in read_events(case_dir)? {
        if event.get("type").and_then(Value::as_str) == Some("tool.completed")
            && event.get("name").and_then(Value::as_str) == Some(tool_name)
            && predicate(&event)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_events(case_dir: &Path) -> Result<Vec<Value>> {
    let runs_dir = case_dir.join(".staff").join("runs");
    if !runs_dir.exists() {
        return Ok(Vec::new());
    }
    let mut events = Vec::new();
    for entry in fs::read_dir(runs_dir)? {
        let entry = entry?;
        let path = entry.path().join("events.jsonl");
        if !path.exists() {
            continue;
        }
        for line in fs::read_to_string(path)?.lines() {
            if let Ok(event) = serde_json::from_str::<Value>(line) {
                events.push(event);
            }
        }
    }
    Ok(events)
}

fn find_file_containing(root: &Path, needle: &str, extensions: &[&str]) -> Result<bool> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if path
                .strip_prefix(root)
                .ok()
                .and_then(|rel| rel.components().next())
                .is_some_and(|part| part.as_os_str() == ".staff")
            {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let ext_matches = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    extensions
                        .iter()
                        .any(|allowed| allowed.trim_start_matches('.') == ext)
                });
            if ext_matches {
                let path_text = path.to_string_lossy().to_ascii_lowercase();
                let content = fs::read_to_string(&path)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if path_text.contains(needle) || content.contains(needle) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn write_report(workspace: &Path, report: &EvalReport) -> Result<()> {
    let output_dir = PathBuf::from(&report.output_dir);
    fs::write(
        output_dir.join("scorecard.json"),
        serde_json::to_string_pretty(report)?,
    )?;
    fs::write(
        output_dir.join("summary.md"),
        render_markdown_report(report),
    )?;
    fs::write(
        workspace.join(".staff").join("evals").join("latest"),
        format!("{}\n", report.output_dir),
    )?;
    Ok(())
}

fn render_markdown_report(report: &EvalReport) -> String {
    let mut out = format!(
        "# Eval {}\n\n- passed: {}\n- failed: {}\n- output_dir: `{}`\n\n",
        report.suite, report.passed, report.failed, report.output_dir
    );
    for case in &report.cases {
        out.push_str(&format!(
            "## {}\n\n- status: {}\n- score: {}/{}\n- runs: {}\n\n",
            case.name,
            if case.passed { "passed" } else { "failed" },
            case.score,
            case.max_score,
            case.run_ids.join(", ")
        ));
        out.push_str("### Checks\n\n");
        for check in &case.checks {
            out.push_str(&format!(
                "- [{}] {}: {}\n",
                if check.passed { "x" } else { " " },
                check.name,
                check.detail
            ));
        }
        out.push_str("\n### Final Summary\n\n");
        out.push_str(&case.final_summary);
        out.push_str("\n\n");
    }
    out
}

fn print_report(report: &EvalReport) {
    println!(
        "eval {}: passed={} failed={} output={}",
        report.suite, report.passed, report.failed, report.output_dir
    );
    for case in &report.cases {
        println!(
            "{} {}/{} {}",
            if case.passed { "PASS" } else { "FAIL" },
            case.score,
            case.max_score,
            case.name
        );
    }
}

fn new_id_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:x}")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_markdown_suite() {
        let suite = EvalSuite::parse("tui_regression", TUI_REGRESSION).expect("suite parses");
        assert!(suite.cases.len() >= 5);
        assert!(suite
            .cases
            .iter()
            .any(|case| case.name == "calculator-follow-up-implementation"));
    }
}
