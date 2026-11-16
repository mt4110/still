use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::Terminal;
use std::io::{self, Stdout};
use std::path::PathBuf;

use crate::plan::io::{load_plan, save_plan};
use crate::tui::render::ui;
use crate::tui::state::AppState;

pub struct App {
    plan_path: PathBuf,
    state: AppState,
}

impl App {
    pub fn new(plan_path: PathBuf) -> Result<Self> {
        let plan = load_plan(&plan_path)?;
        Ok(Self {
            plan_path,
            state: AppState::new(plan),
        })
    }

    pub fn run(&mut self) -> Result<()> {
        let mut terminal = setup_terminal()?;
        let res = run_app(&mut terminal, &mut self.state);
        restore_terminal(&mut terminal)?;

        // Save plan on exit if modified? For MVP yes, auto-save selected actions.
        if res.is_ok() {
            save_plan(&self.state.plan, &self.plan_path)?;
            println!("Plan updated and saved to {:?}", self.plan_path);
        }

        res
    }
}

pub(crate) fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(err) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(err.into());
    }
    let backend = CrosstermBackend::new(stdout);
    match Terminal::new(backend) {
        Ok(terminal) => Ok(terminal),
        Err(err) => {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
            Err(err.into())
        }
    }
}

pub(crate) fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    let raw = disable_raw_mode();
    let screen = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    raw?;
    screen?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, state: &mut AppState) -> Result<()> {
    use crate::plan::ActionType;
    use crate::tui::state::AppMode;

    while !state.should_quit {
        terminal.draw(|f| ui(f, state))?;

        if event::poll(std::time::Duration::from_millis(
            crate::policy::TUI_REFRESH_MS,
        ))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match state.mode {
                        AppMode::Normal => match key.code {
                            KeyCode::Char('q') => state.should_quit = true,
                            KeyCode::Down | KeyCode::Char('j') => state.next(),
                            KeyCode::Up | KeyCode::Char('k') => state.previous(),
                            KeyCode::Char(' ') => state.cycle_action(),
                            KeyCode::Char('/') => {
                                state.mode = AppMode::Filtering;
                            }
                            KeyCode::Char('s') => {
                                state.sort = state.sort.next();
                                state.update_view();
                            }
                            KeyCode::Char('b') => {
                                state.mode = AppMode::BulkConfirm;
                            }
                            KeyCode::Char('K') => state.set_current_action(ActionType::Keep),
                            KeyCode::Char('Q') => state.set_current_action(ActionType::Quarantine),
                            KeyCode::Char('T') => state.set_current_action(ActionType::Trash),
                            _ => {}
                        },
                        AppMode::Filtering => match key.code {
                            KeyCode::Esc | KeyCode::Enter => {
                                state.mode = AppMode::Normal;
                            }
                            KeyCode::Backspace => {
                                state.filter.text.pop();
                                state.update_view();
                            }
                            KeyCode::Char(c)
                                if state.filter.text.len()
                                    < crate::policy::TUI_FILTER_QUERY_MAX_LEN =>
                            {
                                state.filter.text.push(c);
                                state.update_view();
                            }
                            _ => {}
                        },
                        AppMode::BulkConfirm => match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => {
                                state.mode = AppMode::Normal;
                            }
                            KeyCode::Char('K') => {
                                state.bulk_apply(ActionType::Keep);
                                state.mode = AppMode::Normal;
                            }
                            KeyCode::Char('Q') => {
                                state.bulk_apply(ActionType::Quarantine);
                                state.mode = AppMode::Normal;
                            }
                            KeyCode::Char('T') => {
                                state.bulk_apply(ActionType::Trash);
                                state.mode = AppMode::Normal;
                            }
                            _ => {}
                        },
                    }
                }
            }
        }
    }
    Ok(())
}
