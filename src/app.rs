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
    time_window::{self, CalendarScale, Mode, PeriodKey},
    tui,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum View {
    Dashboard,
    CalendarOverview,
    CalendarDetail,
}

pub struct CalendarState {
    pub scale: CalendarScale,
    pub selected: PeriodKey,
    pub visible_periods: Vec<PeriodKey>,
}

pub struct AppState {
    pub config: Config,
    pub view: View,
    pub mode: Mode,
    pub stats: HashMap<Mode, UsageStats>,
    pub loading: HashSet<Mode>,
    pub calendar: CalendarState,
    pub calendar_costs: HashMap<PeriodKey, f64>,
    pub calendar_loading: bool,
    pub history_stats: HashMap<PeriodKey, UsageStats>,
    pub history_loading: HashSet<PeriodKey>,
    pub error: Option<String>,
    pub last_refresh_started: Option<DateTime<Local>>,
    pub next_refresh_due: Instant,
}

impl AppState {
    fn new(config: Config) -> Result<Self> {
        let next_refresh_due = Instant::now() + config.refresh_interval;
        let selected =
            time_window::current_period(CalendarScale::Day, Local::now(), config.daily_start)?;
        let visible_periods = time_window::visible_periods(selected, config.daily_start)?;

        Ok(Self {
            config,
            view: View::Dashboard,
            mode: Mode::Daily,
            stats: HashMap::new(),
            loading: HashSet::new(),
            calendar: CalendarState {
                scale: CalendarScale::Day,
                selected,
                visible_periods,
            },
            calendar_costs: HashMap::new(),
            calendar_loading: false,
            history_stats: HashMap::new(),
            history_loading: HashSet::new(),
            error: None,
            last_refresh_started: None,
            next_refresh_due,
        })
    }

    pub fn current_stats(&self) -> Option<&UsageStats> {
        self.stats.get(&self.mode)
    }

    pub fn is_current_loading(&self) -> bool {
        self.loading.contains(&self.mode)
    }

    pub fn selected_history_stats(&self) -> Option<&UsageStats> {
        self.history_stats.get(&self.calendar.selected)
    }

    pub fn is_selected_history_loading(&self) -> bool {
        self.history_loading.contains(&self.calendar.selected)
    }

    pub fn calendar_cost(&self, period: PeriodKey) -> Option<f64> {
        self.calendar_costs.get(&period).copied()
    }

    fn switch_mode(&mut self, mode: Mode, tx: &Sender<RefreshMessage>) {
        self.view = View::Dashboard;
        self.mode = mode;
        if !self.stats.contains_key(&mode) {
            self.trigger_dashboard_refresh(tx);
        }
    }

    fn trigger_current_refresh(&mut self, tx: &Sender<RefreshMessage>) {
        match self.view {
            View::Dashboard => self.trigger_dashboard_refresh(tx),
            View::CalendarOverview => self.trigger_calendar_refresh(tx),
            View::CalendarDetail => self.trigger_history_refresh(tx),
        }
    }

    fn trigger_dashboard_refresh(&mut self, tx: &Sender<RefreshMessage>) {
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
            let result = refresh_dashboard(config, mode).map_err(|error| format!("{error:#}"));
            let _ = tx.send(RefreshMessage::Dashboard { mode, result });
        });
    }

    fn enter_calendar(&mut self, tx: &Sender<RefreshMessage>) {
        self.view = View::CalendarOverview;
        self.error = None;
        self.ensure_calendar_costs(tx);
    }

    fn open_calendar_detail(&mut self, tx: &Sender<RefreshMessage>) {
        self.view = View::CalendarDetail;
        self.error = None;
        if !self.history_stats.contains_key(&self.calendar.selected) {
            self.trigger_history_refresh(tx);
        }
    }

    fn set_calendar_scale(
        &mut self,
        scale: CalendarScale,
        tx: &Sender<RefreshMessage>,
    ) -> Result<()> {
        let selected_start = local_from_millis(self.calendar.selected.start_millis)?;
        self.calendar.scale = scale;
        self.calendar.selected =
            time_window::current_period(scale, selected_start, self.config.daily_start)?;
        self.sync_visible_periods()?;
        self.ensure_calendar_costs(tx);
        if self.view == View::CalendarDetail {
            self.trigger_history_refresh(tx);
        }
        Ok(())
    }

    fn move_calendar_selection(&mut self, steps: i32, tx: &Sender<RefreshMessage>) -> Result<()> {
        self.calendar.selected = time_window::shift_period(self.calendar.selected, steps)?;
        self.sync_visible_periods()?;
        self.ensure_calendar_costs(tx);
        if self.view == View::CalendarDetail {
            self.trigger_history_refresh(tx);
        }
        Ok(())
    }

    fn sync_visible_periods(&mut self) -> Result<()> {
        self.calendar.visible_periods =
            time_window::visible_periods(self.calendar.selected, self.config.daily_start)?;
        Ok(())
    }

    fn ensure_calendar_costs(&mut self, tx: &Sender<RefreshMessage>) {
        if self
            .calendar
            .visible_periods
            .iter()
            .any(|period| !self.calendar_costs.contains_key(period))
        {
            self.trigger_calendar_refresh(tx);
        }
    }

    fn trigger_calendar_refresh(&mut self, tx: &Sender<RefreshMessage>) {
        if self.calendar_loading {
            return;
        }

        let periods = self.calendar.visible_periods.clone();
        self.calendar_loading = true;
        self.last_refresh_started = Some(Local::now());
        self.error = None;

        let tx = tx.clone();
        let config = self.config.clone();
        thread::spawn(move || {
            let result = db::load_period_costs(&config.db_path, &periods)
                .map_err(|error| format!("{error:#}"));
            let _ = tx.send(RefreshMessage::Calendar { result });
        });
    }

    fn trigger_history_refresh(&mut self, tx: &Sender<RefreshMessage>) {
        let period = self.calendar.selected;
        if self.history_loading.contains(&period) {
            return;
        }

        self.history_loading.insert(period);
        self.last_refresh_started = Some(Local::now());
        self.error = None;

        let tx = tx.clone();
        let config = self.config.clone();
        thread::spawn(move || {
            let result = db::load_usage_between(
                &config.db_path,
                period.mode(),
                period.start_millis,
                period.end_millis,
            )
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(RefreshMessage::History { period, result });
        });
    }

    fn apply_dashboard_refresh(
        &mut self,
        mode: Mode,
        result: std::result::Result<UsageStats, String>,
    ) {
        self.loading.remove(&mode);
        match result {
            Ok(stats) => {
                self.stats.insert(mode, stats);
                if self.view == View::Dashboard && mode == self.mode {
                    self.error = None;
                }
            }
            Err(error) => {
                if self.view == View::Dashboard && mode == self.mode {
                    self.error = Some(error);
                }
            }
        }
    }

    fn apply_calendar_refresh(
        &mut self,
        result: std::result::Result<Vec<db::PeriodCost>, String>,
        tx: &Sender<RefreshMessage>,
    ) {
        self.calendar_loading = false;
        match result {
            Ok(costs) => {
                for cost in costs {
                    self.calendar_costs.insert(cost.period, cost.cost);
                }
                if self.view == View::CalendarOverview {
                    self.error = None;
                }
            }
            Err(error) => {
                if self.view == View::CalendarOverview {
                    self.error = Some(error);
                }
            }
        }
        self.ensure_calendar_costs(tx);
    }

    fn apply_history_refresh(
        &mut self,
        period: PeriodKey,
        result: std::result::Result<UsageStats, String>,
    ) {
        self.history_loading.remove(&period);
        match result {
            Ok(stats) => {
                self.history_stats.insert(period, stats);
                if self.view == View::CalendarDetail && period == self.calendar.selected {
                    self.error = None;
                }
            }
            Err(error) => {
                if self.view == View::CalendarDetail && period == self.calendar.selected {
                    self.error = Some(error);
                }
            }
        }
    }
}

enum RefreshMessage {
    Dashboard {
        mode: Mode,
        result: std::result::Result<UsageStats, String>,
    },
    Calendar {
        result: std::result::Result<Vec<db::PeriodCost>, String>,
    },
    History {
        period: PeriodKey,
        result: std::result::Result<UsageStats, String>,
    },
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
    let mut app = AppState::new(config)?;
    app.trigger_dashboard_refresh(&tx);

    loop {
        drain_refreshes(&rx, &mut app, &tx);
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
        KeyCode::Char('q') => true,
        KeyCode::Esc => match app.view {
            View::Dashboard => true,
            View::CalendarOverview => {
                app.view = View::Dashboard;
                app.error = None;
                false
            }
            View::CalendarDetail => {
                app.view = View::CalendarOverview;
                app.error = None;
                false
            }
        },
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => true,
        KeyCode::Char('c') if app.view == View::Dashboard => {
            app.enter_calendar(tx);
            false
        }
        KeyCode::Tab => {
            match app.view {
                View::Dashboard => app.switch_mode(app.mode.next(), tx),
                View::CalendarOverview | View::CalendarDetail => {
                    apply_calendar_action(app, tx, |app, tx| {
                        app.set_calendar_scale(app.calendar.scale.next(), tx)
                    });
                }
            }
            false
        }
        KeyCode::BackTab => {
            match app.view {
                View::Dashboard => app.switch_mode(app.mode.previous(), tx),
                View::CalendarOverview | View::CalendarDetail => {
                    apply_calendar_action(app, tx, |app, tx| {
                        app.set_calendar_scale(app.calendar.scale.previous(), tx)
                    });
                }
            }
            false
        }
        KeyCode::Char('r') => {
            app.trigger_current_refresh(tx);
            app.next_refresh_due = Instant::now() + app.config.refresh_interval;
            false
        }
        KeyCode::Enter | KeyCode::Char(' ') if app.view == View::CalendarOverview => {
            app.open_calendar_detail(tx);
            false
        }
        _ => {
            if app.view == View::CalendarOverview {
                if let Some(steps) = overview_steps(app.calendar.scale, code) {
                    apply_calendar_action(app, tx, |app, tx| {
                        app.move_calendar_selection(steps, tx)
                    });
                    return false;
                }
            }

            if app.view == View::CalendarDetail {
                if let Some(steps) = detail_steps(code) {
                    apply_calendar_action(app, tx, |app, tx| {
                        app.move_calendar_selection(steps, tx)
                    });
                    return false;
                }
            }

            false
        }
    }
}

fn handle_mouse(mouse: MouseEvent, area: Rect, app: &mut AppState, tx: &Sender<RefreshMessage>) {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return;
    }

    if let Some(target) = tui::tab_at_position(mouse.column, mouse.row, area) {
        match target {
            tui::TabTarget::Mode(mode) => match app.view {
                View::Dashboard => app.switch_mode(mode, tx),
                View::CalendarOverview | View::CalendarDetail => {
                    if let Some(scale) = CalendarScale::from_mode(mode) {
                        apply_calendar_action(app, tx, |app, tx| app.set_calendar_scale(scale, tx));
                    } else {
                        app.switch_mode(mode, tx);
                    }
                }
            },
            tui::TabTarget::Calendar => {
                if app.view == View::Dashboard {
                    app.enter_calendar(tx);
                } else {
                    app.view = View::Dashboard;
                    app.error = None;
                    if !app.stats.contains_key(&app.mode) {
                        app.trigger_dashboard_refresh(tx);
                    }
                }
            }
        }
    }
}

fn drain_refreshes(rx: &Receiver<RefreshMessage>, app: &mut AppState, tx: &Sender<RefreshMessage>) {
    while let Ok(message) = rx.try_recv() {
        match message {
            RefreshMessage::Dashboard { mode, result } => app.apply_dashboard_refresh(mode, result),
            RefreshMessage::Calendar { result } => app.apply_calendar_refresh(result, tx),
            RefreshMessage::History { period, result } => app.apply_history_refresh(period, result),
        }
    }
}

fn maybe_auto_refresh(app: &mut AppState, tx: &Sender<RefreshMessage>) {
    if !app.config.auto_refresh || Instant::now() < app.next_refresh_due {
        return;
    }

    app.trigger_current_refresh(tx);
    app.next_refresh_due = Instant::now() + app.config.refresh_interval;
}

fn refresh_dashboard(config: Config, mode: Mode) -> Result<UsageStats> {
    let cutoff_millis = time_window::cutoff_millis(mode, Local::now(), config.daily_start)?;
    db::load_usage(&config.db_path, mode, cutoff_millis)
}

fn overview_steps(scale: CalendarScale, code: KeyCode) -> Option<i32> {
    match code {
        KeyCode::Left | KeyCode::Char('h') => Some(-1),
        KeyCode::Right | KeyCode::Char('l') => Some(1),
        KeyCode::Up | KeyCode::Char('k') => Some(-overview_columns(scale)),
        KeyCode::Down | KeyCode::Char('j') => Some(overview_columns(scale)),
        _ => None,
    }
}

fn overview_columns(scale: CalendarScale) -> i32 {
    match scale {
        CalendarScale::Day => 7,
        CalendarScale::Week => 4,
        CalendarScale::Month => 3,
    }
}

fn detail_steps(code: KeyCode) -> Option<i32> {
    match code {
        KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => Some(-1),
        KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') => Some(1),
        _ => None,
    }
}

fn apply_calendar_action(
    app: &mut AppState,
    tx: &Sender<RefreshMessage>,
    action: impl FnOnce(&mut AppState, &Sender<RefreshMessage>) -> Result<()>,
) {
    if let Err(error) = action(app, tx) {
        app.error = Some(format!("{error:#}"));
    }
}

fn local_from_millis(millis: i64) -> Result<DateTime<Local>> {
    DateTime::from_timestamp_millis(millis)
        .map(|value| value.with_timezone(&Local))
        .ok_or_else(|| anyhow::anyhow!("timestamp is outside the supported range"))
}
