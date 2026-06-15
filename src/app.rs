use std::{
    collections::{HashMap, HashSet},
    io,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, layout::Rect, Terminal};

use crate::{
    config::Config,
    db::{self, UsageStats},
    time_window::{self, Mode},
    tui,
};

pub struct AppState {
    pub config: Config,
    pub mode: Mode,
    pub stats: HashMap<Mode, UsageStats>,
    pub loading: HashSet<Mode>,
    pub error: Option<String>,
    pub last_refresh_started: Option<DateTime<Local>>,
    pub next_refresh_due: Instant,
}

impl AppState {
    fn new(config: Config) -> Self {
        let next_refresh_due = Instant::now() + config.refresh_interval;
        Self {
            config,
            mode: Mode::Daily,
            stats: HashMap::new(),
            loading: HashSet::new(),
            error: None,
            last_refresh_started: None,
            next_refresh_due,
        }
    }

    pub fn current_stats(&self) -> Option<&UsageStats> {
        self.stats.get(&self.mode)
    }

    pub fn is_current_loading(&self) -> bool {
        self.loading.contains(&self.mode)
    }

    fn switch_mode(&mut self, mode: Mode, tx: &Sender<RefreshMessage>) {
        self.mode = mode;
        if !self.stats.contains_key(&mode) {
            self.trigger_refresh(tx);
        }
    }

    fn trigger_refresh(&mut self, tx: &Sender<RefreshMessage>) {
        if self.loading.contains(&self.mode) {
            return;
        }

        let mode = self.mode;
        self.loading.insert(mode);
        self.last_refresh_started = Some(Local::now());
        self.error = None;

        let tx = tx.clone();
        let config = self.config.clone();
        thread::spawn(move || {
            let result = refresh(config, mode).map_err(|error| format!("{error:#}"));
            let _ = tx.send(RefreshMessage { mode, result });
        });
    }

    fn apply_refresh(&mut self, message: RefreshMessage) {
        self.loading.remove(&message.mode);
        match message.result {
            Ok(stats) => {
                self.stats.insert(message.mode, stats);
                if message.mode == self.mode {
                    self.error = None;
                }
            }
            Err(error) => {
                if message.mode == self.mode {
                    self.error = Some(error);
                }
            }
        }
    }
}

struct RefreshMessage {
    mode: Mode,
    result: std::result::Result<UsageStats, String>,
}

pub fn run(config: Config) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(&mut terminal, config);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    result
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, config: Config) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut app = AppState::new(config);
    app.trigger_refresh(&tx);

    loop {
        drain_refreshes(&rx, &mut app);
        maybe_auto_refresh(&mut app, &tx);

        terminal.draw(|frame| tui::draw(frame, &app))?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if handle_key(key.code, key.modifiers, &mut app, &tx) {
                        return Ok(());
                    }
                }
                Event::Mouse(mouse) => {
                    let size = terminal.size()?;
                    let area = Rect::new(0, 0, size.width, size.height);
                    handle_mouse(mouse, area, &mut app, &tx);
                }
                _ => {}
            }
        }
    }
}

fn handle_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    app: &mut AppState,
    tx: &Sender<RefreshMessage>,
) -> bool {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => true,
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => true,
        KeyCode::Tab => {
            app.switch_mode(app.mode.next(), tx);
            false
        }
        KeyCode::BackTab => {
            app.switch_mode(app.mode.previous(), tx);
            false
        }
        KeyCode::Char('r') => {
            app.trigger_refresh(tx);
            app.next_refresh_due = Instant::now() + app.config.refresh_interval;
            false
        }
        _ => false,
    }
}

fn handle_mouse(mouse: MouseEvent, area: Rect, app: &mut AppState, tx: &Sender<RefreshMessage>) {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return;
    }

    if let Some(mode) = tui::mode_at_tab_position(mouse.column, mouse.row, area) {
        app.switch_mode(mode, tx);
    }
}

fn drain_refreshes(rx: &Receiver<RefreshMessage>, app: &mut AppState) {
    while let Ok(message) = rx.try_recv() {
        app.apply_refresh(message);
    }
}

fn maybe_auto_refresh(app: &mut AppState, tx: &Sender<RefreshMessage>) {
    if !app.config.auto_refresh || Instant::now() < app.next_refresh_due {
        return;
    }

    app.trigger_refresh(tx);
    app.next_refresh_due = Instant::now() + app.config.refresh_interval;
}

fn refresh(config: Config, mode: Mode) -> Result<UsageStats> {
    let cutoff_millis = time_window::cutoff_millis(mode, Local::now(), config.daily_start)?;
    db::load_usage(&config.db_path, mode, cutoff_millis)
}
