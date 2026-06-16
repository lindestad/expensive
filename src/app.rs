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
    config::{self, ColorTheme, Config, ThemeScope},
    db::{self, UsageStats},
    time_window::{self, CalendarScale, Mode, PeriodKey, WeekStart},
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigNotice {
    pub message: String,
    pub is_error: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigEditorItem {
    AutoRefresh,
    WeekStart,
    ColorTheme,
    ThemeScope,
}

pub const CONFIG_EDITOR_VISIBLE_ROWS: usize = 4;

impl ConfigEditorItem {
    pub const ALL: [Self; 4] = [
        Self::AutoRefresh,
        Self::WeekStart,
        Self::ColorTheme,
        Self::ThemeScope,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::AutoRefresh => "auto_refresh",
            Self::WeekStart => "week_start",
            Self::ColorTheme => "color_theme",
            Self::ThemeScope => "theme_scope",
        }
    }
}

pub struct AppState {
    pub config: Config,
    pub view: View,
    pub show_help: bool,
    pub config_selection: usize,
    pub config_scroll: usize,
    pub config_notice: Option<ConfigNotice>,
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
        let selected = time_window::current_period(
            CalendarScale::Day,
            Local::now(),
            config.daily_start,
            config.week_start,
        )?;
        let visible_periods =
            time_window::visible_periods(selected, config.daily_start, config.week_start)?;

        Ok(Self {
            config,
            view: View::Dashboard,
            show_help: false,
            config_selection: 0,
            config_scroll: 0,
            config_notice: None,
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

    pub fn selected_config_item(&self) -> ConfigEditorItem {
        ConfigEditorItem::ALL[self.config_selection.min(ConfigEditorItem::ALL.len() - 1)]
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
        self.calendar.selected = time_window::current_period(
            scale,
            selected_start,
            self.config.daily_start,
            self.config.week_start,
        )?;
        self.sync_visible_periods()?;
        match self.view {
            View::Dashboard => {}
            View::CalendarOverview => self.ensure_calendar_costs(tx),
            View::CalendarDetail => {
                self.ensure_calendar_costs(tx);
                self.trigger_history_refresh(tx);
            }
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

    fn select_calendar_period(
        &mut self,
        period: PeriodKey,
        tx: &Sender<RefreshMessage>,
    ) -> Result<()> {
        self.calendar.scale = period.scale;
        self.calendar.selected = period;
        self.sync_visible_periods()?;
        self.ensure_calendar_costs(tx);
        Ok(())
    }

    fn sync_visible_periods(&mut self) -> Result<()> {
        self.calendar.visible_periods = time_window::visible_periods(
            self.calendar.selected,
            self.config.daily_start,
            self.config.week_start,
        )?;
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

    fn move_config_selection(&mut self, steps: i32) {
        let len = ConfigEditorItem::ALL.len() as i32;
        let idx = self.config_selection as i32;
        self.config_selection = (idx + steps).rem_euclid(len) as usize;
        self.ensure_config_selection_visible();
    }

    fn ensure_config_selection_visible(&mut self) {
        let max_scroll = ConfigEditorItem::ALL
            .len()
            .saturating_sub(CONFIG_EDITOR_VISIBLE_ROWS);
        self.config_scroll = self.config_scroll.min(max_scroll);

        if self.config_selection < self.config_scroll {
            self.config_scroll = self.config_selection;
        } else if self.config_selection >= self.config_scroll + CONFIG_EDITOR_VISIBLE_ROWS {
            self.config_scroll = self
                .config_selection
                .saturating_add(1)
                .saturating_sub(CONFIG_EDITOR_VISIBLE_ROWS);
        }
    }

    fn edit_selected_config(&mut self, direction: i32, tx: &Sender<RefreshMessage>) -> Result<()> {
        match self.selected_config_item() {
            ConfigEditorItem::AutoRefresh => {
                self.config.auto_refresh = !self.config.auto_refresh;
                if self.config.auto_refresh {
                    self.next_refresh_due = Instant::now() + self.config.refresh_interval;
                }
            }
            ConfigEditorItem::WeekStart => {
                self.config.week_start = cycle_week_start(self.config.week_start, direction);
                self.realign_calendar_for_config(tx)?;
            }
            ConfigEditorItem::ColorTheme => {
                self.config.color_theme =
                    cycle_value(&ColorTheme::ALL, self.config.color_theme, direction);
            }
            ConfigEditorItem::ThemeScope => {
                self.config.theme_scope = cycle_value(
                    &[ThemeScope::Calendar, ThemeScope::All],
                    self.config.theme_scope,
                    direction,
                );
            }
        }

        self.save_config_notice();
        Ok(())
    }

    fn realign_calendar_for_config(&mut self, tx: &Sender<RefreshMessage>) -> Result<()> {
        let selected_start = local_from_millis(self.calendar.selected.start_millis)?;
        self.calendar.selected = time_window::current_period(
            self.calendar.scale,
            selected_start,
            self.config.daily_start,
            self.config.week_start,
        )?;
        self.sync_visible_periods()?;
        self.ensure_calendar_costs(tx);
        if self.view == View::CalendarDetail {
            self.trigger_history_refresh(tx);
        }
        Ok(())
    }

    fn save_config_notice(&mut self) {
        match config::save(&self.config) {
            Ok(()) => {
                self.config_notice = Some(ConfigNotice {
                    message: "saved config".to_string(),
                    is_error: false,
                });
            }
            Err(error) => {
                self.config_notice = Some(ConfigNotice {
                    message: format!("config not saved: {error:#}"),
                    is_error: true,
                });
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
    if code == KeyCode::Char('?') {
        app.show_help = !app.show_help;
        return false;
    }

    if app.show_help {
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Up | KeyCode::Char('k') => {
                app.move_config_selection(-1);
                return false;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                app.move_config_selection(1);
                return false;
            }
            KeyCode::BackTab => {
                app.move_config_selection(-1);
                return false;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                apply_config_action(app, tx, |app, tx| app.edit_selected_config(-1, tx));
                return false;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter | KeyCode::Char(' ') => {
                apply_config_action(app, tx, |app, tx| app.edit_selected_config(1, tx));
                return false;
            }
            KeyCode::Esc => {
                app.show_help = false;
                return false;
            }
            _ => return false,
        }
    }

    match code {
        KeyCode::Char('q') => true,
        KeyCode::Esc => match app.view {
            View::Dashboard => true,
            View::CalendarOverview => {
                app.show_help = false;
                app.view = View::Dashboard;
                app.error = None;
                false
            }
            View::CalendarDetail => {
                app.show_help = false;
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
    if app.show_help {
        match mouse.kind {
            MouseEventKind::ScrollUp => app.move_config_selection(-1),
            MouseEventKind::ScrollDown => app.move_config_selection(1),
            _ => {}
        }
        return;
    }

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
        return;
    }

    if let Some(period) = tui::calendar_period_at_position(mouse.column, mouse.row, area, app) {
        apply_calendar_action(app, tx, |app, tx| {
            app.select_calendar_period(period, tx)?;
            app.open_calendar_detail(tx);
            Ok(())
        });
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
    let cutoff_millis =
        time_window::cutoff_millis(mode, Local::now(), config.daily_start, config.week_start)?;
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

fn apply_config_action(
    app: &mut AppState,
    tx: &Sender<RefreshMessage>,
    action: impl FnOnce(&mut AppState, &Sender<RefreshMessage>) -> Result<()>,
) {
    if let Err(error) = action(app, tx) {
        app.config_notice = Some(ConfigNotice {
            message: format!("config not applied: {error:#}"),
            is_error: true,
        });
    }
}

fn cycle_week_start(current: WeekStart, direction: i32) -> WeekStart {
    cycle_value(&[WeekStart::Monday, WeekStart::Sunday], current, direction)
}

fn cycle_value<T: Copy + Eq>(values: &[T], current: T, direction: i32) -> T {
    let current_idx = values
        .iter()
        .position(|value| *value == current)
        .unwrap_or(0) as i32;
    let next_idx = (current_idx + direction.signum()).rem_euclid(values.len() as i32) as usize;
    values[next_idx]
}

fn local_from_millis(millis: i64) -> Result<DateTime<Local>> {
    DateTime::from_timestamp_millis(millis)
        .map(|value| value.with_timezone(&Local))
        .ok_or_else(|| anyhow::anyhow!("timestamp is outside the supported range"))
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use chrono::TimeZone;

    use crate::{
        config::Scope,
        time_window::{DailyStart, WeekStart},
    };

    use super::*;

    #[test]
    fn help_space_toggles_auto_refresh_and_saves_config() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path.clone())).unwrap();
        app.show_help = true;
        let (tx, _rx) = mpsc::channel();

        let should_quit = handle_key(KeyCode::Char(' '), KeyModifiers::NONE, &mut app, &tx);

        assert!(!should_quit);
        assert!(!app.config.auto_refresh);
        assert_eq!(
            app.config_notice.as_ref().map(|notice| notice.is_error),
            Some(false)
        );
        assert!(fs::read_to_string(config_path)
            .unwrap()
            .contains("auto_refresh = false"));
    }

    #[test]
    fn help_mouse_wheel_moves_config_selection() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path)).unwrap();
        app.show_help = true;
        let (tx, _rx) = mpsc::channel();

        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 120, 24),
            &mut app,
            &tx,
        );

        assert_eq!(app.selected_config_item(), ConfigEditorItem::WeekStart);

        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 120, 24),
            &mut app,
            &tx,
        );

        assert_eq!(app.selected_config_item(), ConfigEditorItem::AutoRefresh);
    }

    #[test]
    fn help_cycles_selected_option_and_saves_config() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path.clone())).unwrap();
        app.show_help = true;
        let (tx, _rx) = mpsc::channel();

        handle_key(KeyCode::Down, KeyModifiers::NONE, &mut app, &tx);
        handle_key(KeyCode::Down, KeyModifiers::NONE, &mut app, &tx);
        handle_key(KeyCode::Right, KeyModifiers::NONE, &mut app, &tx);

        assert_eq!(app.selected_config_item(), ConfigEditorItem::ColorTheme);
        assert_eq!(app.config.color_theme, ColorTheme::Ember);
        assert!(fs::read_to_string(config_path)
            .unwrap()
            .contains(r#"color_theme = "ember""#));
    }

    #[test]
    fn clicking_calendar_period_selects_and_opens_detail() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path)).unwrap();
        let selected = time_window::current_period(
            CalendarScale::Day,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            DailyStart::default(),
            WeekStart::default(),
        )
        .unwrap();
        app.view = View::CalendarOverview;
        app.calendar.scale = CalendarScale::Day;
        app.calendar.selected = selected;
        app.calendar.visible_periods =
            time_window::visible_periods(selected, DailyStart::default(), WeekStart::default())
                .unwrap();
        for period in &app.calendar.visible_periods {
            app.calendar_costs.insert(*period, 0.0);
        }
        let (tx, _rx) = mpsc::channel();

        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: 7,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 120, 24),
            &mut app,
            &tx,
        );

        assert_eq!(app.view, View::CalendarDetail);
        assert_eq!(app.calendar.selected, selected);
    }

    fn test_config(config_path: PathBuf) -> Config {
        Config {
            db_path: PathBuf::from("/tmp/opencode.db"),
            config_path: Some(config_path),
            daily_start: DailyStart::default(),
            week_start: WeekStart::default(),
            refresh_interval: Duration::from_secs(60),
            auto_refresh: true,
            scope: Scope::All,
            color_theme: ColorTheme::Aurora,
            theme_scope: ThemeScope::Calendar,
        }
    }
}
