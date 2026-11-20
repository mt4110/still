use super::{artifacts, bytes, candidates, filesystem, history, inventory};
use crate::tui::app::{restore_terminal, setup_terminal};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use inventory::{display, Inventory, Row};
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row as TableRow, Table, TableState, Wrap};
use ratatui::Terminal;
use std::collections::{hash_map::Entry, HashMap};
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

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Total,
    Largest,
    Growth,
}

struct ScanHistory {
    store: history::History,
    previous: HashMap<PathBuf, Option<history::Snapshot>>,
}

impl ScanHistory {
    fn visit(&mut self, root: &Path, inventory: &Inventory, record: bool) -> Result<()> {
        if let Entry::Vacant(entry) = self.previous.entry(root.to_owned()) {
            entry.insert(self.store.previous(root)?);
        }
        if record {
            self.store.record(root, inventory)?;
        }
        Ok(())
    }
}

struct Explorer {
    roots: Vec<PathBuf>,
    saved_roots: usize,
    root: usize,
    location: PathBuf,
    inventory: Inventory,
    policy: artifacts::Policy,
    sources: candidates::Sources,
    findings: Option<candidates::Findings>,
    file_candidates: Option<Vec<artifacts::FileCandidate>>,
    external_rows: Vec<candidates::DisplayRow>,
    threads: u16,
    view: View,
    input: Option<(Input, String)>,
    sort: Sort,
    history: Option<ScanHistory>,
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
    history: Option<history::History>,
    sources: candidates::Sources,
) -> Result<()> {
    let location = roots[0].clone();
    eprintln!("Scanning {}", display(&location));
    let inventory = inventory::scan(&location, threads, query.as_deref())?;
    let mut explorer = Explorer {
        saved_roots: roots.len(),
        roots,
        root: 0,
        location,
        inventory,
        policy,
        sources,
        findings: None,
        file_candidates: None,
        external_rows: Vec::new(),
        threads,
        view: if query.is_some() {
            View::Search
        } else {
            View::Tree
        },
        input: None,
        sort: Sort::Total,
        history: history.map(|store| ScanHistory {
            store,
            previous: HashMap::new(),
        }),
        rows: Vec::new(),
        selection: TableState::default(),
        approved: Vec::new(),
        results: Vec::new(),
        message: String::new(),
    };
    let mut terminal = setup_terminal()?;
    if let Some(history) = &mut explorer.history {
        if let Err(err) = history.visit(&explorer.location, &explorer.inventory, query.is_none()) {
            explorer.message = format!("Scan history unavailable: {err:#}");
        }
    }
    explorer.update_rows();
    let res = explorer.event_loop(&mut terminal);
    let restored = restore_terminal(&mut terminal);
    res.and(restored)
}

impl Explorer {
    fn update_rows(&mut self) {
        let mut rows = match self.view {
            View::Tree => self
                .inventory
                .directories
                .iter()
                .filter(|row| row.path.parent() == Some(self.location.as_path()))
                .cloned()
                .collect(),
            View::Search => self.inventory.matches.clone(),
            View::Candidates => self
                .file_candidates
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|candidate| candidate.row.clone())
                .collect(),
            View::Confirm | View::Results => Vec::new(),
        };
        if self.sort == Sort::Growth && self.previous_scan().is_none() {
            self.sort = Sort::Total;
        }
        if self.view == View::Candidates {
            self.external_rows = self
                .findings
                .as_ref()
                .map(|findings| findings.rows(&self.inventory))
                .unwrap_or_default();
        } else {
            match self.sort {
                Sort::Total | Sort::Largest => {
                    inventory::sort_rows(&mut rows, self.sort == Sort::Largest)
                }
                Sort::Growth => rows.sort_unstable_by(|a, b| {
                    let size = |row| self.delta(row).map(history::Delta::signed_bytes);
                    size(b)
                        .cmp(&size(a))
                        .then_with(|| b.usage.size().cmp(&a.usage.size()))
                        .then_with(|| a.path.cmp(&b.path))
                }),
            }
        }
        self.rows = rows;
        self.selection = TableState::default();
        if self.row_count() > 0 {
            self.selection.select(Some(0));
        }
    }

    fn tracks_history(&self) -> bool {
        self.root < self.saved_roots && self.history.is_some()
    }

    fn previous_scan(&self) -> Option<&history::Snapshot> {
        if !self.tracks_history() {
            return None;
        }
        self.history
            .as_ref()?
            .previous
            .get(&self.roots[self.root])?
            .as_ref()
    }

    fn delta(&self, row: &Row) -> Option<history::Delta> {
        let previous = self.previous_scan()?;
        let row = if self.view == View::Search && row.is_dir {
            // Use the root-level inventory so hard-link attribution uses the same root.
            let index = self
                .inventory
                .directories
                .binary_search_by(|dir| dir.path.cmp(&row.path))
                .ok()?;
            &self.inventory.directories[index]
        } else {
            row
        };
        previous.delta(row)
    }

    fn row_count(&self) -> usize {
        match self.view {
            View::Confirm => self.approved.len(),
            View::Results => self.results.len(),
            View::Candidates => self.rows.len() + self.external_rows.len(),
            _ => self.rows.len(),
        }
    }

    fn switch(&mut self, view: View) {
        self.view = view;
        self.update_rows();
    }

    fn reload(&mut self, root: PathBuf, saved_root: bool) -> Result<()> {
        self.approved.clear();
        self.findings = None;
        self.file_candidates = None;
        self.external_rows.clear();
        let new_inventory = inventory::scan(&root, self.threads, None)?;
        self.message.clear();
        if saved_root {
            if let Some(history) = &mut self.history {
                if let Err(err) = history.visit(&root, &new_inventory, true) {
                    self.message = format!("Scan history unavailable: {err:#}");
                }
            }
        }
        self.location = root;
        self.inventory = new_inventory;
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
                KeyCode::Char('r') if self.view == View::Candidates => {
                    self.open_candidates(terminal, true)?;
                }
                KeyCode::Char('r') if self.view != View::Confirm => {
                    self.refresh(terminal, self.root)?;
                }
                KeyCode::Char('y') if self.view == View::Confirm => {
                    self.message = "Applying the reviewed selection…".into();
                    self.draw(terminal)?;
                    self.apply();
                }
                _ if matches!(self.view, View::Confirm | View::Results) => {}
                KeyCode::Tab if self.view == View::Candidates => self.switch(View::Tree),
                KeyCode::Tab => self.open_candidates(terminal, false)?,
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
                KeyCode::Char('s') if self.view != View::Candidates => {
                    self.sort = match self.sort {
                        Sort::Total => Sort::Largest,
                        Sort::Largest if self.previous_scan().is_some() => Sort::Growth,
                        Sort::Largest | Sort::Growth => Sort::Total,
                    };
                    self.update_rows();
                }
                KeyCode::Char('f') if self.view == View::Candidates && !self.rows.is_empty() => {
                    self.selection.select(Some(0));
                }
                KeyCode::Char(key @ ('g' | 'd' | 'K')) if self.view == View::Candidates => {
                    let kind = match key {
                        'g' => "Git",
                        'K' => "Kubernetes",
                        _ => "Docker",
                    };
                    if let Some(index) = self.external_rows.iter().position(|row| row.kind == kind)
                    {
                        self.selection.select(Some(self.rows.len() + index));
                    } else {
                        self.message = format!("No {kind} entries available");
                    }
                }
                KeyCode::Char('o') => self.input = Some((Input::Root, String::new())),
                KeyCode::Char('/') => self.input = Some((Input::Search, String::new())),
                KeyCode::Char(' ') => {
                    self.message =
                        "Checking selected output contents; large folders may take time…".into();
                    self.draw(terminal)?;
                    self.toggle_selection();
                }
                KeyCode::Char('D') if !self.approved.is_empty() => self.switch(View::Confirm),
                _ => {}
            }
        }
    }

    fn open_candidates<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        refresh: bool,
    ) -> Result<()> {
        let mut scan_note = String::new();
        if refresh {
            let root = self.roots[self.root].clone();
            self.message = format!("Scanning {}", display(&root));
            self.draw(terminal)?;
            if let Err(error) = self.reload(root, self.root < self.saved_roots) {
                self.message = format!("{error:#}");
                self.update_rows();
                return Ok(());
            }
            scan_note = std::mem::take(&mut self.message);
        }
        if refresh || self.file_candidates.is_none() {
            self.message = "Checking cleanup candidates…".into();
            self.draw(terminal)?;
            self.file_candidates = Some(artifacts::file_candidates(&self.inventory, &self.policy));
            if self.sources.configured(&self.policy) && (refresh || self.findings.is_none()) {
                self.findings = Some(self.sources.collect(&self.policy));
            }
        }
        self.message = scan_note;
        self.switch(View::Candidates);
        Ok(())
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
                    Input::Root => match inventory::workspace_dir(Path::new(&value)) {
                        Ok(root) => {
                            self.message = format!("Scanning {}", display(&root));
                            self.draw(terminal)?;
                            match self.reload(root.clone(), false) {
                                Ok(()) => {
                                    self.roots.push(root);
                                    self.root = self.roots.len() - 1;
                                    self.switch(View::Tree);
                                }
                                Err(err) => self.message = format!("{err:#}"),
                            }
                        }
                        Err(err) => self.message = format!("Cannot open directory: {err}"),
                    },
                    Input::Search if !value.is_empty() => {
                        self.message = format!("Searching {}", display(&self.location));
                        self.draw(terminal)?;
                        match inventory::workspace_dir(&self.location)
                            .and_then(|root| inventory::scan(&root, self.threads, Some(&value)))
                        {
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
        match self.reload(self.roots[root].clone(), root < self.saved_roots) {
            Ok(()) => {
                self.root = root;
                self.switch(View::Tree);
            }
            Err(err) => self.message = format!("{err:#}"),
        }
        Ok(())
    }

    fn selected(&self) -> Option<&Row> {
        self.selection.selected().and_then(|i| self.rows.get(i))
    }

    fn selected_external(&self) -> Option<&candidates::DisplayRow> {
        if self.view != View::Candidates {
            return None;
        }
        let index = self.selection.selected()?.checked_sub(self.rows.len())?;
        self.external_rows.get(index)
    }

    fn selected_file_candidate(&self) -> Option<&artifacts::FileCandidate> {
        if self.view != View::Candidates {
            return None;
        }
        self.file_candidates
            .as_ref()?
            .get(self.selection.selected()?)
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
        if let Some(row) = self.selected_external() {
            self.message = if !row.eligible {
                format!("Protected: {}", row.reason)
            } else if row.kind == "Git" {
                "Use still worktrees --remove to review this removal".into()
            } else if row.kind == "Kubernetes" {
                "Use still kube to inspect this workload".into()
            } else {
                "Use still docker --remove to review this removal".into()
            };
            return;
        }
        let Some(path) = self.selected().map(|row| row.path.clone()) else {
            self.message.clear();
            return;
        };
        if let Some(index) = self.approved.iter().position(|item| item.path == path) {
            self.approved.remove(index);
            self.message = "Removed from the selection".into();
            return;
        }
        if let Some(reason) = self
            .selected_file_candidate()
            .and_then(|row| row.blocked_reason.as_ref())
        {
            self.message = format!("Blocked: {reason}");
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
                .constraints([Constraint::Length(3), Constraint::Min(3), Constraint::Length(if self.view == View::Candidates { 7 } else { 4 })])
                .split(frame.size());
            let title = match self.view {
                View::Tree => "Storage",
                View::Search => "Search results",
                View::Candidates => "Cleanup candidates",
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
                View::Candidates => self.file_candidates.as_deref().unwrap_or_default().iter().map(|candidate| {
                    let row = &candidate.row;
                    let path = row.path.strip_prefix(&self.roots[self.root]).unwrap_or(&row.path);
                    let selected = if self.approved.iter().any(|item| item.path == row.path) { "✓ " } else { "" };
                    let size = row.usage.allocated_bytes.map_or_else(
                        || format!("{} logical", bytes(row.usage.logical_bytes)), bytes);
                    let label = match &candidate.blocked_reason {
                        Some(reason) => format!("[Blocked: {reason}] {}", display(path)),
                        None => format!("{selected}[FS] {}", display(path)),
                    };
                    TableRow::new([Cell::from(size), Cell::from(label)])
                }).chain(self.external_rows.iter().map(|row| {
                    let size = row.estimated_bytes.map_or_else(|| "—".into(), bytes);
                    let status = if row.eligible { row.kind.to_owned() } else { format!("{} blocked: {}", row.kind, row.reason) };
                    TableRow::new([Cell::from(size), Cell::from(format!("[{status}] {}", row.label))])
                })).skip(first).take(visible).collect(),
                _ => self.rows.iter().skip(first).take(visible).map(|row| {
                    let size = match self.sort {
                        Sort::Total => bytes(row.usage.size()),
                        Sort::Largest => bytes(row.usage.largest_file_bytes),
                        Sort::Growth => self.delta(row).map_or_else(|| "—".into(), history::Delta::label),
                    };
                    let selected = if self.approved.iter().any(|item| item.path == row.path) { "✓ " } else { "" };
                    let partial = if row.usage.skipped == 0 { "" } else { " [partial]" };
                    let path = if self.view == View::Tree {
                        row.path.file_name().map(Path::new).unwrap_or(&row.path)
                    } else {
                        row.path.strip_prefix(&self.roots[self.root]).unwrap_or(&row.path)
                    };
                    TableRow::new([Cell::from(size), Cell::from(format!("{selected}{}{partial}", display(path)))])
                }).collect(),
            };
            let header = match self.view {
                View::Candidates => "Observed",
                View::Tree | View::Search if self.sort == Sort::Growth => "Change",
                View::Tree | View::Search if self.sort == Sort::Largest => "Largest file",
                View::Tree | View::Search if cfg!(unix) => "Allocated",
                View::Tree | View::Search => "Logical size",
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
            } else if self.view == View::Candidates {
                "f Files · g Git · d Docker · K Kubernetes · Space Select output · D Review · r Recheck · Tab Back"
            } else {
                "↑↓ Move · Enter Open · ← Parent · / Search · o Root · ] Next root\nTab Candidates · s Sort · Space Select · D Review · r Refresh · q Quit"
            };
            let footer = if let Some((mode, value)) = &self.input {
                let label = match mode { Input::Root => "Directory", Input::Search => "Search" };
                format!("{label}: {value}\nEnter Continue · Esc Cancel")
            } else if self.view == View::Candidates {
                let detail = if let Some(row) = self.selected_external() {
                    format!("{}\n{}", row.reason, row.estimate_basis)
                } else if let Some(candidate) = self.selected_file_candidate() {
                    let row = &candidate.row;
                    let reason = candidate.producer.reason();
                    let observed = row.usage.allocated_bytes.map_or_else(
                        || format!("{} logical size; host reclaim estimate unavailable", bytes(row.usage.logical_bytes)),
                        |size| format!("{} observed allocation; actual reclaim may differ", bytes(size)));
                    match &candidate.blocked_reason {
                        Some(blocked) => format!("{}\nBlocked: {blocked}\n{observed}", display(&row.path)),
                        None => format!("{reason}\n{observed}"),
                    }
                } else {
                    "No cleanup candidates in this view".into()
                };
                let notes = self.findings.as_ref().map(|findings| findings.notes.join(" · ")).unwrap_or_default();
                [help, detail.as_str(), self.message.as_str(), notes.as_str()]
                    .into_iter().filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\n")
            } else {
                let detail = if self.view == View::Confirm {
                    self.selection.selected().and_then(|i| self.approved.get(i))
                        .map_or_else(String::new, |item| display(&item.path))
                } else {
                    self.message.clone()
                };
                let baseline = if self.tracks_history() && matches!(self.view, View::Tree | View::Candidates | View::Search) {
                    match self.previous_scan() {
                        Some(scan) => {
                            let time = scan.completed_at().with_timezone(&chrono::Local);
                            format!("Previous scan {} · * partial change", time.format("%Y-%m-%d %H:%M"))
                        }
                        None if self.message.starts_with("Scan history unavailable") => String::new(),
                        None => "First scan · Changes appear on the next review".into(),
                    }
                } else {
                    String::new()
                };
                [baseline.as_str(), detail.as_str(), help]
                    .into_iter().filter(|line| !line.is_empty()).collect::<Vec<_>>().join("\n")
            };
            frame.render_widget(Paragraph::new(footer).wrap(Wrap { trim: false }), area[2]);
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use std::fs;

    #[test]
    fn candidate_refresh_rescans_sizes_and_new_outputs_and_drops_old_approvals() -> Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().canonicalize()?;
        let existing = root.join("existing/generated");
        let added = root.join("added/target");
        fs::create_dir_all(&existing)?;
        fs::write(existing.join("artifact.bin"), b"before")?;
        let policy = artifacts::Policy {
            roots: vec![root.clone()],
            outputs: vec![existing.clone()],
            docker_context: None,
        };
        let approved = policy.prepare(&existing)?;
        let mut explorer = Explorer {
            roots: vec![root.clone()],
            saved_roots: 1,
            root: 0,
            location: root.join("existing"),
            inventory: inventory::scan(&root, 2, None)?,
            policy,
            sources: candidates::Sources {
                git_repos: Vec::new(),
                git_base: None,
                docker_cache: false,
                kube: None,
            },
            findings: Some(candidates::Findings::default()),
            file_candidates: None,
            external_rows: Vec::new(),
            threads: 2,
            view: View::Tree,
            input: None,
            sort: Sort::Total,
            history: None,
            rows: Vec::new(),
            selection: TableState::default(),
            approved: vec![approved],
            results: Vec::new(),
            message: String::new(),
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 30))?;
        explorer.open_candidates(&mut terminal, false)?;
        assert_eq!(explorer.rows.len(), 1);
        assert_eq!(explorer.rows[0].usage.logical_bytes, 6);

        fs::write(existing.join("artifact.bin"), [0u8; 8192])?;
        fs::create_dir_all(added.join("debug"))?;
        fs::write(
            root.join("added/Cargo.toml"),
            "[package]\nname = 'fixture'\n",
        )?;
        fs::write(added.join(".rustc_info.json"), b"{}")?;
        fs::write(added.join("debug/artifact.bin"), [0u8; 16384])?;
        explorer.open_candidates(&mut terminal, true)?;

        assert!(explorer.view == View::Candidates);
        assert_eq!(explorer.roots, vec![root.clone()]);
        assert_eq!((explorer.root, explorer.saved_roots), (0, 1));
        assert_eq!(explorer.rows.len(), 2);
        assert!(explorer
            .rows
            .iter()
            .any(|row| row.path == existing && row.usage.logical_bytes == 8192));
        assert!(explorer
            .rows
            .iter()
            .any(|row| row.path == added && row.usage.logical_bytes == 16386));
        assert!(explorer.approved.is_empty());
        assert!(explorer.findings.is_none());
        assert!(explorer.external_rows.is_empty());
        assert!(existing.is_dir() && added.is_dir());
        Ok(())
    }
}
