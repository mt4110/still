use super::{artifacts, bytes, candidate_rows, filesystem, inventory};
use crate::tui::app::{restore_terminal, setup_terminal};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use inventory::{display, Inventory, Row};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row as TableRow, Table, TableState, Wrap};
use ratatui::Terminal;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq)]
enum View {
    Tree,
    Search,
    Candidates,
    Confirm,
    Results,
}

#[derive(Clone, Copy)]
enum Input {
    Root,
    Search,
}

struct Explorer {
    roots: Vec<PathBuf>,
    root: usize,
    location: PathBuf,
    inventory: Inventory,
    policy: artifacts::Policy,
    threads: u16,
    view: View,
    input: Option<(Input, String)>,
    largest: bool,
    rows: Vec<Row>,
    selection: TableState,
    approved: Vec<artifacts::Prepared>,
    results: Vec<String>,
    message: String,
}

pub fn run(
    roots: Vec<PathBuf>,
    threads: u16,
    query: Option<String>,
    policy: artifacts::Policy,
) -> Result<()> {
    let location = roots[0].clone();
    eprintln!("Scanning {}", display(&location));
    let inventory = inventory::scan(&location, threads, query.as_deref())?;
    let mut explorer = Explorer {
        roots,
        root: 0,
        location,
        inventory,
        policy,
        threads,
        view: if query.is_some() {
            View::Search
        } else {
            View::Tree
        },
        input: None,
        largest: false,
        rows: Vec::new(),
        selection: TableState::default(),
        approved: Vec::new(),
        results: Vec::new(),
        message: String::new(),
    };
    explorer.update_rows();
    let mut terminal = setup_terminal()?;
    let res = explorer.event_loop(&mut terminal);
    let restored = restore_terminal(&mut terminal);
    res.and(restored)
}

impl Explorer {
    fn update_rows(&mut self) {
        self.rows = match self.view {
            View::Tree => self
                .inventory
                .directories
                .iter()
                .filter(|row| row.path.parent() == Some(self.location.as_path()))
                .cloned()
                .collect(),
            View::Search => self.inventory.matches.clone(),
            View::Candidates => candidate_rows(&self.inventory, &self.policy),
            View::Confirm | View::Results => Vec::new(),
        };
        inventory::sort_rows(&mut self.rows, self.largest);
        self.selection = TableState::default();
        if self.row_count() > 0 {
            self.selection.select(Some(0));
        }
    }

    fn row_count(&self) -> usize {
        match self.view {
            View::Confirm => self.approved.len(),
            View::Results => self.results.len(),
            _ => self.rows.len(),
        }
    }

    fn switch(&mut self, view: View) {
        self.view = view;
        self.update_rows();
    }

    fn reload(&mut self, root: PathBuf) -> Result<()> {
        let new_inventory = inventory::scan(&root, self.threads, None)?;
        self.location = root;
        self.inventory = new_inventory;
        self.switch(View::Tree);
        Ok(())
    }

    fn event_loop<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        loop {
            self.draw(terminal)?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Ok(());
            }
            if self.input.is_some() {
                self.handle_input(terminal, key.code)?;
                continue;
            }
            match key.code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                KeyCode::PageUp => self.move_selection(-10),
                KeyCode::PageDown => self.move_selection(10),
                KeyCode::Esc if self.view != View::Tree => self.switch(View::Tree),
                KeyCode::Char('r') if self.view != View::Confirm => {
                    self.refresh(terminal, self.root)?;
                }
                KeyCode::Char('y') if self.view == View::Confirm => {
                    self.message = "Applying the reviewed selection…".into();
                    self.draw(terminal)?;
                    self.apply();
                }
                _ if matches!(self.view, View::Confirm | View::Results) => {}
                KeyCode::Tab => self.switch(if self.view == View::Candidates {
                    View::Tree
                } else {
                    View::Candidates
                }),
                KeyCode::Enter => {
                    if let Some(row) = self.selected() {
                        self.location = if row.is_dir {
                            row.path.clone()
                        } else {
                            row.path.parent().unwrap_or(&row.path).to_owned()
                        };
                        self.switch(View::Tree);
                    }
                }
                KeyCode::Backspace | KeyCode::Left if self.location != self.roots[self.root] => {
                    if let Some(parent) = self.location.parent() {
                        self.location = parent.to_owned();
                        self.switch(View::Tree);
                    }
                }
                KeyCode::Char(']') => {
                    self.refresh(terminal, (self.root + 1) % self.roots.len())?;
                }
                KeyCode::Char('s') => {
                    self.largest = !self.largest;
                    self.update_rows();
                }
                KeyCode::Char('o') => self.input = Some((Input::Root, String::new())),
                KeyCode::Char('/') => self.input = Some((Input::Search, String::new())),
                KeyCode::Char(' ') => {
                    self.message = "Checking selected output…".into();
                    self.draw(terminal)?;
                    self.toggle_selection();
                }
                KeyCode::Char('D') if !self.approved.is_empty() => self.switch(View::Confirm),
                _ => {}
            }
        }
    }

    fn handle_input<B: Backend>(&mut self, terminal: &mut Terminal<B>, key: KeyCode) -> Result<()> {
        let Some((mode, buffer)) = &mut self.input else {
            return Ok(());
        };
        match key {
            KeyCode::Esc => self.input = None,
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Char(c) if !c.is_control() => buffer.push(c),
            KeyCode::Enter => {
                let mode = *mode;
                let value = std::mem::take(buffer);
                self.input = None;
                match mode {
                    Input::Root => match std::fs::canonicalize(&value) {
                        Ok(root) => {
                            self.message = format!("Scanning {}", display(&root));
                            self.draw(terminal)?;
                            match self.reload(root.clone()) {
                                Ok(()) => {
                                    self.roots.push(root);
                                    self.root = self.roots.len() - 1;
                                    self.message.clear();
                                }
                                Err(err) => self.message = format!("{err:#}"),
                            }
                        }
                        Err(err) => self.message = format!("Cannot open directory: {err}"),
                    },
                    Input::Search if !value.is_empty() => {
                        self.message = format!("Searching {}", display(&self.location));
                        self.draw(terminal)?;
                        match inventory::scan(&self.location, self.threads, Some(&value)) {
                            Ok(scan) => {
                                // The full inventory is kept for navigation back to the root.
                                self.inventory.matches = scan.matches;
                                self.inventory.omitted_matches = scan.omitted_matches;
                                self.message = format!("Search: {value}");
                                if scan.omitted_matches > 0 {
                                    self.message.push_str(&format!(
                                        " · {} additional matches",
                                        scan.omitted_matches
                                    ));
                                }
                                self.switch(View::Search);
                            }
                            Err(err) => self.message = format!("{err:#}"),
                        }
                    }
                    Input::Search => self.switch(View::Tree),
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn refresh<B: Backend>(&mut self, terminal: &mut Terminal<B>, root: usize) -> Result<()> {
        self.message = format!("Scanning {}", display(&self.roots[root]));
        self.draw(terminal)?;
        match self.reload(self.roots[root].clone()) {
            Ok(()) => {
                self.root = root;
                self.message.clear();
            }
            Err(err) => self.message = format!("{err:#}"),
        }
        Ok(())
    }

    fn selected(&self) -> Option<&Row> {
        self.selection.selected().and_then(|i| self.rows.get(i))
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.row_count();
        if len > 0 {
            let i = self.selection.selected().unwrap_or(0);
            self.selection
                .select(Some(i.saturating_add_signed(delta).min(len - 1)));
        }
    }

    fn toggle_selection(&mut self) {
        let Some(path) = self.selected().map(|row| row.path.clone()) else {
            self.message.clear();
            return;
        };
        if let Some(index) = self.approved.iter().position(|item| item.path == path) {
            self.approved.remove(index);
            self.message = "Removed from the selection".into();
            return;
        }
        if self
            .approved
            .iter()
            .any(|item| path.starts_with(&item.path) || item.path.starts_with(&path))
        {
            self.message = "Overlaps an already selected output".into();
            return;
        }
        match self.policy.prepare(&path) {
            Ok(prepared) => {
                self.message = format!(
                    "Selected · {} · {} files",
                    prepared.producer.reason(),
                    prepared.files()
                );
                self.approved.push(prepared);
            }
            Err(err) => self.message = format!("{err:#}"),
        }
    }

    fn apply(&mut self) {
        self.results.clear();
        for item in self.approved.drain(..) {
            let parent = item.path.parent().unwrap_or(Path::new("."));
            let before = filesystem::available(parent);
            match self.policy.remove(&item) {
                Ok(()) => {
                    let change = match (before, filesystem::available(parent)) {
                        (Some(a), Some(b)) if b >= a => format!(" · free space +{}", bytes(b - a)),
                        (Some(a), Some(b)) => format!(" · free space -{}", bytes(a - b)),
                        _ => String::new(),
                    };
                    self.results
                        .push(format!("Removed {}{change}", display(&item.path)));
                }
                Err(err) => self
                    .results
                    .push(format!("{} · {err:#}", display(&item.path))),
            }
        }
        self.message = "r Refresh storage · Esc Return to the previous inventory".into();
        self.switch(View::Results);
    }

    fn draw<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        terminal.draw(|frame| {
            let area = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(3), Constraint::Length(4)])
                .split(frame.size());
            let title = match self.view {
                View::Tree => "Storage",
                View::Search => "Search results",
                View::Candidates => "Output to review",
                View::Confirm => "Permanently remove these outputs?",
                View::Results => "Cleanup results",
            };
            frame.render_widget(Paragraph::new(display(&self.location))
                .block(Block::default().title(title).borders(Borders::ALL)), area[0]);

            let visible = usize::from(area[1].height.saturating_sub(1)).max(1);
            let offset = self.selection.offset();
            let first = self.selection.selected().map_or(0, |index| {
                offset.min(index).max((index + 1).saturating_sub(visible))
            });
            *self.selection.offset_mut() = first;
            let render_rows: Vec<_> = match self.view {
                View::Confirm => self.approved.iter().skip(first).take(visible).map(|item| {
                    TableRow::new([Cell::from(format!("{} files", item.files())),
                        Cell::from(format!("{} · {}", display(&item.path), item.producer.reason()))])
                }).collect(),
                View::Results => self.results.iter().skip(first).take(visible)
                    .map(|result| TableRow::new([Cell::from(""), Cell::from(result.as_str())]))
                    .collect(),
                _ => self.rows.iter().skip(first).take(visible).map(|row| {
                    let size = if self.largest { row.usage.largest_file_bytes } else { row.usage.size() };
                    let selected = if self.approved.iter().any(|item| item.path == row.path) { "✓ " } else { "" };
                    let partial = if row.usage.skipped == 0 { "" } else { " [partial]" };
                    let path = if self.view == View::Tree {
                        row.path.file_name().map(Path::new).unwrap_or(&row.path)
                    } else {
                        row.path.strip_prefix(&self.roots[self.root]).unwrap_or(&row.path)
                    };
                    TableRow::new([Cell::from(bytes(size)), Cell::from(format!("{selected}{}{partial}", display(path)))])
                }).collect(),
            };
            let header = match self.view {
                View::Tree | View::Candidates | View::Search if self.largest => "Largest file",
                View::Tree | View::Candidates | View::Search if cfg!(unix) => "Allocated",
                View::Tree | View::Candidates | View::Search => "Logical size",
                _ => "",
            };
            // Render the current window only, with a local index into that window.
            let mut window = TableState::default();
            window.select(self.selection.selected().map(|index| index.saturating_sub(first)));
            let size_width = area[1].width.saturating_sub(5).min(15);
            let path_width = area[1].width.saturating_sub(size_width + 3);
            let table = Table::new(render_rows, [Constraint::Length(size_width), Constraint::Length(path_width)])
                .header(TableRow::new([header, "Path"]).style(Style::default().fg(Color::Cyan)))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .block(Block::default().borders(Borders::LEFT | Borders::RIGHT));
            frame.render_stateful_widget(table, area[1], &mut window);

            let help = if self.view == View::Confirm {
                "y Delete the listed outputs · Esc Back"
            } else if self.view == View::Results {
                "r Refresh · Esc Back · q Quit"
            } else {
                "↑↓ Move · Enter Open · ← Parent · / Search · o Root · ] Next root\nTab Candidates · s Sort · Space Select · D Review deletion · r Refresh · q Quit"
            };
            let footer = if let Some((mode, value)) = &self.input {
                let label = match mode { Input::Root => "Directory", Input::Search => "Search" };
                format!("{label}: {value}\nEnter Continue · Esc Cancel")
            } else {
                let detail = if self.view == View::Confirm {
                    self.selection.selected().and_then(|i| self.approved.get(i))
                        .map_or_else(String::new, |item| display(&item.path))
                } else {
                    self.message.clone()
                };
                format!("{help}\n{detail}")
            };
            frame.render_widget(Paragraph::new(footer).wrap(Wrap { trim: false }), area[2]);
        })?;
        Ok(())
    }
}
