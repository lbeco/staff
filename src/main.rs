mod eval;
mod runtime;
mod tui;

use runtime::{Result, StaffError};
use std::env;

fn main() {
    let code = match run() {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("staff: {err}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let workspace = env::current_dir()?;
    if args.is_empty() {
        return tui::run_tui(workspace, None);
    }
    if matches!(args[0].as_str(), "help" | "--help" | "-h") {
        runtime::print_help();
        return Ok(());
    }
    match args[0].as_str() {
        "tui" => tui::run_tui(workspace, parse_tui_prompt(&args[1..])?),
        "exec" => runtime::run_exec_from_args(&workspace, &args[1..]),
        "eval" => eval::run_eval(&workspace, &args[1..]),
        "runs" => runtime::run_runs(&workspace, &args[1..]),
        "tools" => runtime::run_tools(),
        "sandbox" => runtime::run_sandbox(&args[1..]),
        "checkpoint" => runtime::run_checkpoint(&workspace, &args[1..]),
        other => Err(StaffError::new(format!(
            "unknown command `{other}`. Run `staff help`."
        ))),
    }
}

fn parse_tui_prompt(args: &[String]) -> Result<Option<String>> {
    if args.is_empty() {
        return Ok(None);
    }
    let mut prompt = None;
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--prompt" => {
                let Some(value) = args.get(idx + 1) else {
                    return Err(StaffError::new("usage: staff tui --prompt \"<task>\""));
                };
                prompt = Some(value.clone());
                idx += 2;
            }
            other => {
                return Err(StaffError::new(format!(
                    "unknown tui option `{other}`. Usage: staff tui [--prompt \"<task>\"]"
                )));
            }
        }
    }
    Ok(prompt)
}
