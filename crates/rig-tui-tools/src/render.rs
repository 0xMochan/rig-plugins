use bevy_ecs::prelude::*;
use rig_harness::rig_core::tool::OutputCut;
use rig_tui::highlight::{Language, code_rows};
use rig_tui::ratatui::{
    style::{Color, Style, Stylize},
    text::{Line, Span},
};
use rig_tui::{CallState, LookLine, RenderBatch, ToolCallView};
use serde_json::Value;

use crate::{
    DisplaySettings, ScriptTool,
    input::{Fold, Folds},
};

/// Named decoration gives the mouse an identity independent of tool text.
pub(super) const DETAIL: &str = "rig-tui-tools:detail";

pub(super) fn draw(
    mut batch: ResMut<RenderBatch>,
    settings: Res<DisplaySettings>,
    folds: Res<Folds>,
) {
    let (agent, width, interactive) = (batch.agent, batch.width, batch.pane.is_some());
    for (input, output) in batch.calls_mut() {
        let Some(tool) = ScriptTool::named(input.call.function.name.as_str()) else {
            continue;
        };
        // Exports retain all available content independently of interactive folds.
        let fold = if interactive {
            folds.get(agent, &input.call.id)
        } else {
            Fold::Expanded
        };
        let (lines, clipped) = lines(&input.view(), tool, fold, &settings, width);
        output.lines = lines;
        let hint = match (interactive, fold, clipped) {
            (true, Fold::Expanded, _) => Some("Show preview · click or Alt+O".to_owned()),
            (true, _, Some(kind)) => Some(format!("… more {kind} · click or Alt+O show all")),
            _ => None,
        };
        output.decorate(
            DETAIL,
            hint.into_iter()
                .map(|hint| Line::from(format!("   {hint}")).dim())
                .collect(),
        );
    }
}

/// Fold the completed presentation, after specialized renderers and decorators.
/// Script tools already budget their input and output separately in `draw`.
/// Other tools retain their original lines, colors and named decorations.
pub(super) fn fold_tools(
    mut batch: ResMut<RenderBatch>,
    settings: Res<DisplaySettings>,
    folds: Res<Folds>,
) {
    let (agent, width, interactive) = (batch.agent, batch.width, batch.pane.is_some());
    for (input, output) in batch.calls_mut() {
        let view = input.view();
        let script = ScriptTool::named(view.name()).is_some();
        let fold = if interactive {
            folds.get(agent, &input.call.id)
        } else {
            Fold::Expanded
        };
        if !script {
            // The stock fallback escapes multiline arguments into its header
            // and omits typed JSON results. Only replace that fallback, never
            // a tool's own file/diff/image presentation.
            if output.lines == view.default_lines() {
                let detail = ["path", "file_path", "command", "query"]
                    .into_iter()
                    .find_map(|name| view.argument(name))
                    .map(|value| value.lines().next().unwrap_or_default())
                    .unwrap_or_default();
                let summary: String = detail.chars().take(80).collect();
                output.lines = vec![view.header(view.name(), summary).into()];
                if !view.call.function.arguments.is_empty() {
                    json(
                        &mut output.lines,
                        "input",
                        &Value::Object(view.call.function.arguments.clone()),
                    );
                }
                // Keep stock text/progress semantics, then add JSON blocks
                // that the fallback cannot display.
                output.lines.extend(view.result_lines());
                if let Some(result) = view.result {
                    for content in &result.content {
                        if let Some(value) = content.as_json() {
                            json(&mut output.lines, "result", value);
                        }
                    }
                }
            }
            // An intentionally hidden tool stays hidden.
            if output.lines.is_empty() && output.decorations.is_empty() {
                continue;
            }
            let marker = if fold == Fold::Collapsed {
                "▸ "
            } else {
                "▾ "
            };
            if output.lines.is_empty() {
                output.lines.push(view.header(view.name(), "").into());
            }
            let mut header = output.lines.remove(0).colored();
            header.spans.insert(0, Span::from(marker).dim());
            if let Some(cut) = view
                .facts
                .and_then(|facts| facts.get::<OutputCut>().ok().flatten())
            {
                header.spans.push(
                    Span::from(format!(
                        " · output truncated by tool ({} lines omitted)",
                        cut.cut_lines()
                    ))
                    .yellow(),
                );
            } else if view.result.is_some_and(|result| {
                result
                    .content
                    .iter()
                    .filter_map(|content| content.as_json())
                    .any(truncated)
            }) {
                header
                    .spans
                    .push(Span::from(" · output truncated by tool").yellow());
            }
            if fold == Fold::Collapsed && interactive {
                header
                    .spans
                    .push(Span::from(" · click or Ctrl+O open").dim());
            }
            let body = std::mem::take(&mut output.lines);
            output.lines.push(header.into());
            let mut left = usize::from(settings.output_rows.clamp(1, 1000));
            let mut clipped = false;
            if fold != Fold::Collapsed {
                clipped |= append_budget(&mut output.lines, body, &mut left, fold, width);
                for (_, decoration) in &mut output.decorations {
                    let body = std::mem::take(decoration)
                        .into_iter()
                        .map(LookLine::from)
                        .collect();
                    let mut rows = Vec::new();
                    clipped |= append_budget(&mut rows, body, &mut left, fold, width);
                    *decoration = rows.into_iter().map(|row| row.colored()).collect();
                }
            } else {
                output.decorations.clear();
            }
            let hint = match (interactive, fold, clipped) {
                (true, Fold::Collapsed, _) => None,
                (true, Fold::Expanded, _) => Some("Show preview · click or Alt+O"),
                (true, _, true) => Some("… more output · click or Alt+O show all"),
                _ => None,
            };
            output.decorate(
                DETAIL,
                hint.into_iter()
                    .map(|text| Line::from(format!("   {text}")).dim())
                    .collect(),
            );
        }
        // Quiet, terminal-native headers instead of full-width colored bands.
        // Failure/state words remain visible even in a collapsed call.
        if let Some(header) = output.lines.first_mut() {
            let mut line = header.colored();
            line.style.bg = Some(Color::Reset);
            for span in &mut line.spans {
                span.style.bg = Some(Color::Reset);
            }
            *header = line.into();
        }
    }
}

fn lines(
    view: &ToolCallView<'_>,
    tool: ScriptTool,
    fold: Fold,
    settings: &DisplaySettings,
    width: u16,
) -> (Vec<LookLine>, Option<&'static str>) {
    let source = view.argument(tool.source());
    let result = view
        .result
        .filter(|result| result.content.len() == 1)
        .and_then(|result| result.content.first())
        .and_then(|content| content.as_json());
    let mut detail = match tool {
        ScriptTool::Xonsh => format!(
            "{} · {} lines",
            view.argument("backend").unwrap_or("native"),
            source.unwrap_or_default().lines().count()
        ),
        ScriptTool::Monty => format!(
            "python · {} lines",
            source.unwrap_or_default().lines().count()
        ),
        ScriptTool::Native => "native".to_owned(),
        ScriptTool::Virtual => "virtual".to_owned(),
    };
    if let Some(status) = result.and_then(status) {
        detail.push_str(&format!(" · {status}"));
    }
    if let Some(took) = view.took {
        detail.push_str(&format!(" · {:.1}s", took.as_secs_f64()));
    }
    if result.is_some_and(truncated) {
        detail.push_str(" · output truncated by tool");
    }
    match view.state {
        CallState::Streaming => detail.push_str(" · receiving script"),
        CallState::Waiting => detail.push_str(" · waiting"),
        _ => {}
    }
    if fold == Fold::Collapsed {
        if let Some(first) = source.and_then(|source| source.lines().next()) {
            let summary: String = first.chars().take(48).collect();
            detail.push_str(&format!(" · {summary}"));
        }
        detail.push_str(" · click or Ctrl+O open");
    }
    let marker = if fold == Fold::Collapsed {
        "▸"
    } else {
        "▾"
    };
    let mut header = view.header(format!("{marker} {}", view.name()), detail);
    if result.is_some_and(execution_failed) {
        header = header.red();
    }
    let mut rows = vec![header.into()];
    if fold == Fold::Collapsed {
        return (rows, None);
    }
    let input_cut = append(
        &mut rows,
        input_lines(view, tool),
        settings.input_rows,
        fold,
        width,
    );
    let output_cut = append(
        &mut rows,
        output_lines(view, result, fold),
        settings.output_rows,
        fold,
        width,
    );
    // This remains visible even when the result body's preview is exhausted.
    if result.is_some_and(truncated) {
        label(
            &mut rows,
            "Output was truncated by the tool; expansion cannot recover omitted bytes.",
            Style::new().yellow(),
        );
    }
    let clipped = match (input_cut, output_cut) {
        (true, true) => Some("input/output"),
        (true, false) => Some("input"),
        (false, true) => Some("output"),
        (false, false) => None,
    };
    (rows, clipped)
}

fn input_lines(view: &ToolCallView<'_>, tool: ScriptTool) -> Vec<LookLine> {
    let mut inputs = match view.argument(tool.source()) {
        Some(source) => code_rows(source, Language::Named(tool.language())),
        None => code_rows(&view.call.function.raw_arguments(), Language::Named("json")),
    };
    for (name, value) in &view.call.function.arguments {
        if name == tool.source()
            || (name == "backend" && matches!(tool, ScriptTool::Xonsh) && value.is_string())
            || value.is_null()
        {
            continue;
        }
        match (name.as_str(), value) {
            ("files", Value::Object(files)) => {
                for (path, value) in files {
                    label(&mut inputs, &format!("file {path}"), Style::new().dim());
                    match value.as_str() {
                        Some(text) => inputs.extend(code_rows(text, Language::File(path))),
                        None => json(&mut inputs, "invalid file content", value),
                    }
                }
            }
            ("files", Value::Array(files)) => {
                for file in files {
                    if let (Some(path), Some(content)) = (
                        file.get("path").and_then(Value::as_str),
                        file.get("content").and_then(Value::as_str),
                    ) {
                        label(&mut inputs, &format!("file {path}"), Style::new().dim());
                        inputs.extend(code_rows(content, Language::File(path)));
                    } else {
                        json(&mut inputs, "file", file);
                    }
                }
            }
            (_, Value::Object(map)) if map.is_empty() => {}
            (_, Value::Array(items)) if items.is_empty() => {}
            (_, Value::String(text)) => {
                label(&mut inputs, name, Style::new().dim());
                inputs.extend(
                    rig_tui::text_rows(text, Style::default())
                        .into_iter()
                        .map(LookLine::from),
                );
            }
            _ => json(&mut inputs, name, value),
        }
    }
    inputs
}

fn output_lines(view: &ToolCallView<'_>, result: Option<&Value>, fold: Fold) -> Vec<LookLine> {
    let mut outputs = Vec::new();
    if let Some(value) = result {
        if let Some(map) = value.as_object() {
            for (name, value) in map {
                if matches!(
                    name.as_str(),
                    "status"
                        | "exit_code"
                        | "truncated"
                        | "stdout_truncated"
                        | "stderr_truncated"
                        | "output_truncated"
                ) {
                    continue;
                }
                match (name.as_str(), value) {
                    ("stdout" | "stderr" | "output", Value::String(text)) => {
                        if !text.is_empty() {
                            let style = if name == "stderr" {
                                Style::new().yellow()
                            } else {
                                Style::default()
                            };
                            label(&mut outputs, name, style.dim());
                            outputs.extend(
                                rig_tui::text_rows(text, style)
                                    .into_iter()
                                    .map(LookLine::from),
                            );
                        }
                    }
                    ("value", Value::String(text)) => {
                        label(&mut outputs, "value", Style::new().dim());
                        outputs.extend(
                            rig_tui::text_rows(text, Style::default())
                                .into_iter()
                                .map(LookLine::from),
                        );
                    }
                    _ => json(&mut outputs, name, value),
                }
            }
        } else {
            json(&mut outputs, "result", value);
        }
    } else if let Some(result) = view.result {
        let style = if view.failed() {
            Style::new().red()
        } else {
            Style::default()
        };
        label(
            &mut outputs,
            if view.failed() { "error" } else { "output" },
            style.dim(),
        );
        for content in &result.content {
            if let Some(text) = content.as_text() {
                outputs.extend(
                    rig_tui::text_rows(text, style)
                        .into_iter()
                        .map(LookLine::from),
                );
            } else if let Some(value) = content.as_json() {
                json(&mut outputs, "result", value);
            }
        }
    } else {
        if let Some(text) = view.output {
            label(&mut outputs, "output so far", Style::new().dim());
            outputs.extend(
                rig_tui::text_rows(text, Style::default())
                    .into_iter()
                    .map(LookLine::from),
            );
        }
        outputs.extend(view.progress_lines().into_iter().map(LookLine::from));
    }
    if view.result.is_none() || fold == Fold::Expanded {
        for child in view.children {
            if child.state == CallState::Done && !child.failed && fold != Fold::Expanded {
                continue;
            }
            let state = if child.failed {
                "failed".to_owned()
            } else {
                match &child.state {
                    CallState::Streaming => "receiving".to_owned(),
                    CallState::Waiting => "waiting".to_owned(),
                    CallState::Held(holds) => format!("held: {}", holds.join(", ")),
                    CallState::Running(_) => "running".to_owned(),
                    CallState::Done => "done".to_owned(),
                    CallState::Stopped => "stopped".to_owned(),
                }
            };
            label(
                &mut outputs,
                &format!("↳ {} · {state}", child.name),
                if child.failed {
                    Style::new().red()
                } else {
                    Style::new().dim()
                },
            );
            if let Some(call) = &child.call
                && let Some(tool) = ScriptTool::named(call.function.name.as_str())
                && let Some(source) = call
                    .function
                    .arguments
                    .get(tool.source())
                    .and_then(Value::as_str)
            {
                outputs.extend(code_rows(source, Language::Named(tool.language())));
            }
        }
    }
    outputs
}

fn label(rows: &mut Vec<LookLine>, text: &str, style: Style) {
    rows.push(Line::from(format!("   {text}")).style(style).into());
}

fn json(rows: &mut Vec<LookLine>, name: &str, value: &Value) {
    label(rows, name, Style::new().dim());
    // serde_json::Value contains no fallible custom serialization.
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    rows.extend(code_rows(&text, Language::Named("json")));
}

fn status(value: &Value) -> Option<String> {
    if let Some(code) = value.get("exit_code").and_then(Value::as_i64) {
        return Some(format!("exit {code}"));
    }
    let status = value.get("status")?;
    if let Some(code) = status.get("exited").and_then(Value::as_i64) {
        Some(format!("exit {code}"))
    } else {
        status.as_str().map(|text| text.replace('_', " "))
    }
}

fn truncated(value: &Value) -> bool {
    [
        "truncated",
        "output_truncated",
        "stdout_truncated",
        "stderr_truncated",
    ]
    .iter()
    .any(|name| value.get(name).and_then(Value::as_bool) == Some(true))
}

fn execution_failed(value: &Value) -> bool {
    value
        .get("exit_code")
        .and_then(Value::as_i64)
        .is_some_and(|code| code != 0)
        || value.get("status").is_some_and(|status| {
            status
                .as_str()
                .is_some_and(|status| matches!(status, "signaled" | "timed_out" | "stopped"))
                || status
                    .get("exited")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0)
        })
}

fn append(
    rows: &mut Vec<LookLine>,
    body: Vec<LookLine>,
    limit: u16,
    fold: Fold,
    width: u16,
) -> bool {
    if fold == Fold::Expanded {
        rows.extend(body);
        return false;
    }
    let mut left = usize::from(limit.clamp(1, 1000));
    append_budget(rows, body, &mut left, fold, width)
}

fn append_budget(
    rows: &mut Vec<LookLine>,
    body: Vec<LookLine>,
    left: &mut usize,
    fold: Fold,
    width: u16,
) -> bool {
    if fold == Fold::Expanded {
        rows.extend(body);
        return false;
    }
    let mut clipped = false;
    for line in body {
        if *left == 0 {
            clipped = true;
            break;
        }
        // Use Rig's own wrap so a single long line cannot fill many screens.
        let wrapped = rig_tui::wrap::wrap(line.line(), usize::from(width.max(1)));
        if wrapped.len() <= *left {
            *left -= wrapped.len();
            rows.push(line);
        } else {
            let colored = rig_tui::wrap::wrap(&line.colored(), usize::from(width.max(1)));
            rows.extend(
                colored
                    .into_iter()
                    .take(*left)
                    .map(|(line, _)| LookLine::from(line)),
            );
            *left = 0;
            clipped = true;
            break;
        }
    }
    clipped
}
