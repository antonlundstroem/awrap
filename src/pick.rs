//! The skim pickers: one over every app, one over an app's workloads.

use std::borrow::Cow;
use std::env;
use std::ops::Range;

use ratatui::style::Color;
use ratatui::text::{Line, Span};
use skim::prelude::*;

use crate::apps::{App, Workload};
use crate::view;

/// A picker row: `text` is shown and matched, `output` is what picking it returns.
struct Row {
    text: String,
    output: String,
    preview: Option<String>,
    /// Icons shown before `text`, each in its ANSI color; not searched.
    icons: Vec<(char, Option<u8>)>,
    /// Character ranges of `text` shown in an ANSI color.
    colors: Vec<(Range<usize>, u8)>,
}

impl SkimItem for Row {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.text)
    }

    fn display(&self, context: DisplayContext) -> Line<'_> {
        if self.icons.is_empty() && self.colors.is_empty() {
            return context.to_line(Cow::Borrowed(&self.text));
        }
        let colored = |color: Option<u8>| match color {
            Some(color) => context.base_style.fg(Color::Indexed(color)),
            None => context.base_style,
        };
        // Rows with colors are ASCII, so byte and character positions agree.
        let matched = |i: usize| match &context.matches {
            Matches::CharIndices(indices) => indices.contains(&i),
            Matches::CharRange(start, end) | Matches::ByteRange(start, end) => {
                (*start..*end).contains(&i)
            }
            Matches::None => false,
        };
        let mut line = Line::default();
        for &(icon, color) in &self.icons {
            line.spans
                .push(Span::styled(format!("{icon} "), colored(color)));
        }
        for (i, c) in self.text.chars().enumerate() {
            let color = self.colors.iter().find(|(range, _)| range.contains(&i));
            let mut style = colored(color.map(|&(_, color)| color));
            if matched(i) {
                style = style.patch(context.matched_style);
            }
            match line.spans.last_mut() {
                Some(span) if span.style == style => span.content.to_mut().push(c),
                _ => line.spans.push(Span::styled(c.to_string(), style)),
            }
        }
        line
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.output)
    }

    fn preview(&self, _context: PreviewContext) -> ItemPreview {
        match &self.preview {
            Some(command) => ItemPreview::Command(command.clone()),
            None => ItemPreview::Global,
        }
    }
}

/// Names longer than this push the rest of their row right instead of widening
/// every row; 95% of the names in a 600-app instance fit.
const NAME_WIDTH: usize = 56;

/// Picks an app's name, starting from `query`. `None` if left with Esc.
pub fn app(apps: &[App], query: &str) -> Result<Option<String>, String> {
    let width = |column: fn(&App) -> &str| apps.iter().map(|a| column(a).len()).max().unwrap_or(0);
    let (nw, cw, sw) = (
        width(|a| a.name.as_str()).min(NAME_WIDTH),
        width(|a| a.cluster.as_str()),
        width(|a| a.sync.as_str()),
    );
    let line = |name: &str, cluster: &str, sync: &str, health: &str| {
        format!("{name:nw$}  {cluster:cw$}  {sync:sw$}  {health}")
    };
    // The pane runs `aw --preview <app>`, this same binary.
    let aw = env::current_exe().map_err(|e| format!("can't find aw's own path: {e}"))?;
    let aw = sh_quote(&aw.to_string_lossy());
    let rows = apps.iter().map(|a| {
        let text = line(&a.name, &a.cluster, &a.sync, &a.health);
        // The row ends with the sync column, two spaces and the health.
        let health_at = text.len() - a.health.len();
        let sync_at = health_at - 2 - sw;
        let colors = [(sync_at, &a.sync), (health_at, &a.health)]
            .into_iter()
            .filter_map(|(at, status)| Some((at..at + status.len(), view::color(status)?)))
            .collect();
        Row {
            text,
            output: a.name.clone(),
            preview: Some(format!("{aw} --preview {}", sh_quote(&a.name))),
            // In view even when the list is too narrow for the status columns.
            icons: [&a.sync, &a.health]
                .map(|status| (view::icon(status), view::color(status)))
                .to_vec(),
            colors,
        }
    });
    let options = skim_options()
        .query(query)
        // Indented past the rows' two icons.
        .header(format!("    {}", line("APP", "CLUSTER", "SYNC", "HEALTH")))
        // Turns the preview pane on; each row brings its own command.
        .preview("")
        // Beside the list, so the list keeps the full terminal height; rows
        // wider than the list are cut on the right.
        .preview_window("right:50%")
        .build()
        .map_err(|e| e.to_string())?;
    Ok(run(options, rows)?.into_iter().next())
}

/// Picks one or more of `workloads`; Tab marks several. Empty if left with Esc.
pub fn workloads(workloads: &[Workload]) -> Result<Vec<Workload>, String> {
    let kw = workloads.iter().map(|w| w.kind.len()).max().unwrap_or(0);
    let rows = workloads.iter().enumerate().map(|(i, w)| Row {
        text: format!("{:kw$}  {}/{}", w.kind, w.namespace, w.name),
        output: i.to_string(),
        preview: None,
        icons: Vec::new(),
        colors: Vec::new(),
    });
    let options = skim_options()
        .multi(true)
        .header("Tab marks more than one")
        .build()
        .map_err(|e| e.to_string())?;
    let picked = run(options, rows)?;
    Ok(picked
        .iter()
        .map(|i| workloads[i.parse::<usize>().unwrap()].clone())
        .collect())
}

/// What both pickers share: listed top-down, so Tab marks a row and moves to
/// the next; rows never scrolled sideways, so the app name stays in view.
fn skim_options() -> SkimOptionsBuilder {
    let mut options = SkimOptionsBuilder::default();
    options.reverse(true).no_hscroll(true);
    options
}

/// Runs skim over `rows` and returns the outputs of the picked ones.
fn run(options: SkimOptions, rows: impl Iterator<Item = Row>) -> Result<Vec<String>, String> {
    let out = Skim::run_items(options, rows).map_err(|e| e.to_string())?;
    if out.is_abort {
        return Ok(Vec::new());
    }
    Ok(out
        .selected_items
        .iter()
        .map(|m| m.item.output().into_owned())
        .collect())
}

/// `s` quoted for `/bin/sh`, which runs preview commands.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
