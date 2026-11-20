use crate::plan::ActionType;
use crate::tui::state::{AppMode, AppState};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap},
    Frame,
};
use std::path::Path;

pub fn ui(f: &mut Frame, state: &mut AppState) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // List + Details
            Constraint::Length(1), // Status Bar / Help
        ])
        .split(f.size());

    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(main_chunks[0]);

    render_list(f, state, content_chunks[0]);
    render_details(f, state, content_chunks[1]);
    render_status_bar(f, state, main_chunks[1]);

    // Overlay for Input if needed? Or integrated in status bar?
    // Let's integrate in status bar for simplicity, or overlay if it's a modal.
    // If bulk confirmation, maybe a modal.
    if state.mode == AppMode::BulkConfirm {
        render_bulk_modal(f, state);
    }
}

fn render_list(f: &mut Frame, state: &mut AppState, area: Rect) {
    let header_cells = ["Path", "Size", "Action"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().fg(Color::Yellow)));
    let header = Row::new(header_cells)
        .style(Style::default().add_modifier(Modifier::BOLD))
        .height(1)
        .bottom_margin(1);

    // Phase 4: Use view_entries() iterator
    let rows = state
        .view_entries()
        .take(crate::policy::TUI_MAX_RENDER_ROWS)
        .map(|entry| {
            let path_str = prettify_path(&entry.path);
            let size_str = humansize::format_size(entry.size, humansize::DECIMAL);

            let action_style = action_color(entry.selected_action);
            let action_str = format!("{:?}", entry.selected_action);

            let cells = vec![
                Cell::from(path_str),
                Cell::from(size_str),
                Cell::from(action_str).style(action_style),
            ];
            Row::new(cells).height(1).bottom_margin(0)
        });

    let title = format!(
        "Candidates ({} / {})",
        state.view_len(),
        state.plan.entries.len()
    );

    let t = Table::new(
        rows,
        [
            Constraint::Percentage(60),
            Constraint::Length(10),
            Constraint::Length(15),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(title))
    .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol(">> ");

    f.render_stateful_widget(t, area, &mut state.table_state);
}

fn render_details(f: &mut Frame, state: &AppState, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title("Details");

    if let Some(entry) = state.current_entry() {
        let action_desc = match entry.selected_action {
            ActionType::Keep => "Unchanged",
            ActionType::Quarantine => "Moves to Vault (Safe)",
            ActionType::Trash => "Moves to OS Trash (Correct)",
            ActionType::Purge => "BLOCKED (Should not happen)",
        };

        let text = vec![
            Line::from(vec![
                Span::raw("Path: "),
                Span::styled(
                    entry.path.to_string_lossy(),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("Size: "),
                Span::raw(humansize::format_size(entry.size, humansize::DECIMAL)),
            ]),
            Line::from(vec![
                Span::raw("Type: "),
                Span::raw(format!("{:?}", entry.entry_type)),
            ]),
            Line::from(vec![
                Span::raw("Modified: "),
                Span::raw(format!("{:?}", entry.modified)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::raw("Reason: "),
                Span::styled(&entry.reason, Style::default().fg(Color::Magenta)),
            ]),
            Line::from(format!("Recommended: {:?}", entry.recommended_action)),
            Line::from(""),
            Line::from(vec![
                Span::raw("Action: "),
                Span::styled(
                    format!("{:?}", entry.selected_action),
                    action_color(entry.selected_action).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw("Effect: "),
                Span::styled(action_desc, Style::default().fg(Color::Gray)),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "Press <Space> to cycle action, <q> to quit/save",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: true });
        f.render_widget(paragraph, area);
    } else {
        let paragraph = Paragraph::new("No item selected").block(block);
        f.render_widget(paragraph, area);
    }
}

fn render_status_bar(f: &mut Frame, state: &AppState, area: Rect) {
    let mode_str = match state.mode {
        AppMode::Normal => " NORMAL ",
        AppMode::Filtering => " FILTER ",
        AppMode::BulkConfirm => " BULK MODE ",
    };

    let mode_style = match state.mode {
        AppMode::Normal => Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        AppMode::Filtering => Style::default()
            .bg(Color::Yellow)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
        AppMode::BulkConfirm => Style::default()
            .bg(Color::Red)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    };

    let sort_str = format!(" Sort: {:?} (s) ", state.sort);

    // Filter text
    let filter_str = if state.mode == AppMode::Filtering || state.filter.is_active() {
        format!(" Filter: {} ", state.filter.text)
    } else {
        " Filter: [/] ".to_string()
    };

    let help_str = " | <space>:Cycle <b:Bulk> <q:Quit> <?:Help>";

    let left_spans = vec![
        Span::styled(mode_str, mode_style),
        Span::raw(sort_str),
        Span::raw(filter_str),
    ];
    let right_spans = vec![Span::raw(help_str)];

    // Calculate layout
    // actually just joined spans
    let mut spans = left_spans;
    spans.extend(right_spans);

    let line = Line::from(spans);
    let p = Paragraph::new(line).style(Style::default().bg(Color::DarkGray).fg(Color::White));
    f.render_widget(p, area);
}

fn render_bulk_modal(f: &mut Frame, _state: &AppState) {
    let area = centered_rect(60, 20, f.size());
    let block = Block::default()
        .title("Bulk Action (Filtered Items Only)")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black));
    let text = vec![
        Line::from("Apply action to ALL currently matching items?"),
        Line::from(""),
        Line::from(vec![
            Span::raw("Press "),
            Span::styled("K", Style::default().fg(Color::Green)),
            Span::raw(" to Keep All"),
        ]),
        Line::from(vec![
            Span::raw("Press "),
            Span::styled("Q", Style::default().fg(Color::Yellow)),
            Span::raw(" to Quarantine All"),
        ]),
        Line::from(vec![
            Span::raw("Press "),
            Span::styled("T", Style::default().fg(Color::Red)),
            Span::raw(" to Trash All"),
        ]),
        Line::from(""),
        Line::from("Press ESC or q to Cancel"),
    ];
    let p = Paragraph::new(text)
        .block(block)
        .alignment(ratatui::layout::Alignment::Center);

    f.render_widget(Clear, area); // Clear background
    f.render_widget(p, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn action_color(action: ActionType) -> Style {
    match action {
        ActionType::Keep => Style::default(),
        ActionType::Quarantine => Style::default().fg(Color::Green),
        ActionType::Trash => Style::default().fg(Color::Yellow),
        ActionType::Purge => Style::default().fg(Color::Red),
    }
}

pub fn prettify_path(path: &Path) -> String {
    // Replace $HOME prefix with ~ for readability.
    let mut s = path.to_string_lossy().to_string();
    if let Ok(home) = std::env::var("HOME") {
        if s.starts_with(&home) {
            s = s.replacen(&home, "~", 1);
        }
    }

    // Truncate long paths to keep the table readable.
    const MAX: usize = 64;
    if s.len() > MAX {
        let tail_len = MAX.saturating_sub(1);
        let tail = s.chars().rev().take(tail_len).collect::<Vec<_>>();
        let tail = tail.into_iter().rev().collect::<String>();
        format!("…{}", tail)
    } else {
        s
    }
}
