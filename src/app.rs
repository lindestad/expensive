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
    config::{self, ColorTheme, Config, Scope, ThemeScope},
    db::{self, UsageStats},
    time_window::{self, CalendarScale, DailyStart, Mode, PeriodKey, WeekStart},
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

pub struct ScopePickerState {
    pub selection: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigNotice {
    pub message: String,
    pub is_error: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigEditorItem {
    AutoRefresh,
    DailyStart,
    RefreshSeconds,
    WeekStart,
    ColorTheme,
    ThemeScope,
}

impl ConfigEditorItem {
    pub const ALL: [Self; 6] = [
        Self::AutoRefresh,
        Self::DailyStart,
        Self::RefreshSeconds,
        Self::WeekStart,
        Self::ColorTheme,
        Self::ThemeScope,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::AutoRefresh => "auto_refresh",
            Self::DailyStart => "daily_start",
            Self::RefreshSeconds => "refresh_seconds",
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
    pub scope_picker: Option<ScopePickerState>,
    pub projects: Vec<db::ProjectInfo>,
    pub help_scroll: usize,
    pub dashboard_model_scroll: usize,
    pub history_model_scroll: usize,
    pub dashboard_token_bucket: Option<usize>,
    pub history_token_bucket: Option<usize>,
    pub token_graph_dragging: bool,
    pub config_selection: usize,
    pub config_notice: Option<ConfigNotice>,
    pub mode: Mode,
    pub stats: HashMap<Mode, UsageStats>,
    pub loading: HashSet<Mode>,
    pub graph_loading: HashSet<Mode>,
    pub graph_refresh_pending: HashSet<Mode>,
    pub dashboard_prefetch_attempted: HashSet<Mode>,
    pub calendar: CalendarState,
    pub calendar_costs: HashMap<PeriodKey, f64>,
    pub calendar_loading: bool,
    pub history_stats: HashMap<PeriodKey, UsageStats>,
    pub history_loading: HashSet<PeriodKey>,
    pub history_graph_loading: HashSet<PeriodKey>,
    pub history_graph_refresh_pending: HashSet<PeriodKey>,
    pub error: Option<String>,
    pub last_refresh_started: Option<DateTime<Local>>,
    pub next_refresh_due: Instant,
    pub(crate) request_generation: u64,
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
        let projects = db::list_projects(&config.db_path).unwrap_or_default();

        Ok(Self {
            config,
            view: View::Dashboard,
            show_help: false,
            scope_picker: None,
            projects,
            help_scroll: 0,
            dashboard_model_scroll: 0,
            history_model_scroll: 0,
            dashboard_token_bucket: None,
            history_token_bucket: None,
            token_graph_dragging: false,
            config_selection: 0,
            config_notice: None,
            mode: Mode::Daily,
            stats: HashMap::new(),
            loading: HashSet::new(),
            graph_loading: HashSet::new(),
            graph_refresh_pending: HashSet::new(),
            dashboard_prefetch_attempted: HashSet::new(),
            calendar: CalendarState {
                scale: CalendarScale::Day,
                selected,
                visible_periods,
            },
            calendar_costs: HashMap::new(),
            calendar_loading: false,
            history_stats: HashMap::new(),
            history_loading: HashSet::new(),
            history_graph_loading: HashSet::new(),
            history_graph_refresh_pending: HashSet::new(),
            error: None,
            last_refresh_started: None,
            next_refresh_due,
            request_generation: 0,
        })
    }

    pub fn current_stats(&self) -> Option<&UsageStats> {
        self.stats.get(&self.mode)
    }

    pub fn is_current_loading(&self) -> bool {
        self.loading.contains(&self.mode)
    }

    pub fn is_current_graph_loading(&self) -> bool {
        self.graph_loading.contains(&self.mode)
    }

    pub fn selected_history_stats(&self) -> Option<&UsageStats> {
        self.history_stats.get(&self.calendar.selected)
    }

    pub fn is_selected_history_loading(&self) -> bool {
        self.history_loading.contains(&self.calendar.selected)
    }

    pub fn is_selected_history_graph_loading(&self) -> bool {
        self.history_graph_loading.contains(&self.calendar.selected)
    }

    pub fn calendar_cost(&self, period: PeriodKey) -> Option<f64> {
        self.calendar_costs.get(&period).copied()
    }

    pub fn selected_config_item(&self) -> ConfigEditorItem {
        ConfigEditorItem::ALL[self.config_selection.min(ConfigEditorItem::ALL.len() - 1)]
    }

    fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
        self.scope_picker = None;
        if self.show_help {
            self.help_scroll = 0;
            self.config_selection = 0;
        }
    }

    pub fn scope_label(&self) -> String {
        match &self.config.scope {
            Scope::All => "all projects".to_string(),
            Scope::Current => self
                .projects
                .iter()
                .filter(|project| {
                    std::path::Path::new(&self.config.current_directory)
                        .starts_with(std::path::Path::new(&project.worktree))
                })
                .max_by_key(|project| std::path::Path::new(&project.worktree).components().count())
                .map(|project| project.name.clone())
                .unwrap_or_else(|| "current project".to_string()),
            Scope::Project(id) => self
                .projects
                .iter()
                .find(|project| &project.id == id)
                .map(|project| project.name.clone())
                .unwrap_or_else(|| id.clone()),
        }
    }

    fn open_scope_picker(&mut self) {
        if let Ok(projects) = db::list_projects(&self.config.db_path) {
            self.projects = projects;
        }
        let selection = match &self.config.scope {
            Scope::All => 0,
            Scope::Current => 1,
            Scope::Project(id) => self
                .projects
                .iter()
                .position(|project| &project.id == id)
                .map(|index| index + 2)
                .unwrap_or(0),
        };
        self.show_help = false;
        self.scope_picker = Some(ScopePickerState { selection });
    }

    fn move_scope_selection(&mut self, direction: i32) {
        let option_count = self.projects.len().saturating_add(2);
        let Some(picker) = self.scope_picker.as_mut() else {
            return;
        };
        if direction > 0 {
            picker.selection = (picker.selection + 1).min(option_count.saturating_sub(1));
        } else {
            picker.selection = picker.selection.saturating_sub(1);
        }
    }

    fn apply_scope_selection(&mut self, tx: &Sender<RefreshMessage>) {
        let Some(picker) = self.scope_picker.take() else {
            return;
        };
        let scope = match picker.selection {
            0 => Scope::All,
            1 => Scope::Current,
            index => match self.projects.get(index - 2) {
                Some(project) => Scope::Project(project.id.clone()),
                None => return,
            },
        };
        if self.config.scope == scope {
            return;
        }

        self.config.scope = scope;
        self.request_generation = self.request_generation.wrapping_add(1);
        self.stats.clear();
        self.loading.clear();
        self.graph_loading.clear();
        self.graph_refresh_pending.clear();
        self.dashboard_prefetch_attempted.clear();
        self.calendar_costs.clear();
        self.calendar_loading = false;
        self.history_stats.clear();
        self.history_loading.clear();
        self.history_graph_loading.clear();
        self.history_graph_refresh_pending.clear();
        self.dashboard_token_bucket = None;
        self.history_token_bucket = None;
        self.error = None;
        self.save_config_notice();
        self.trigger_current_refresh(tx);
    }

    fn switch_mode(&mut self, mode: Mode, tx: &Sender<RefreshMessage>) {
        self.view = View::Dashboard;
        if self.mode != mode {
            self.dashboard_model_scroll = 0;
            self.dashboard_token_bucket = None;
        }
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
        self.trigger_dashboard_refresh_for_mode(self.mode, tx, true);
    }

    fn trigger_dashboard_refresh_for_mode(
        &mut self,
        mode: Mode,
        tx: &Sender<RefreshMessage>,
        foreground: bool,
    ) {
        if self.loading.contains(&mode) {
            return;
        }

        self.loading.insert(mode);
        if foreground {
            self.dashboard_prefetch_attempted.clear();
            self.last_refresh_started = Some(Local::now());
            self.error = None;
        }

        let tx = tx.clone();
        let config = self.config.clone();
        let generation = self.request_generation;
        thread::spawn(move || {
            let result =
                refresh_dashboard_summary(config, mode).map_err(|error| format!("{error:#}"));
            let _ = tx.send(RefreshMessage::Dashboard {
                generation,
                mode,
                result,
            });
        });
    }

    fn trigger_dashboard_graph_refresh(&mut self, mode: Mode, tx: &Sender<RefreshMessage>) {
        if self.graph_loading.contains(&mode) || !self.graph_refresh_pending.contains(&mode) {
            return;
        }
        let Some(stats) = self.stats.get(&mode) else {
            return;
        };
        if stats.totals.messages == 0 {
            self.graph_refresh_pending.remove(&mode);
            return;
        }

        let cutoff_millis = stats.cutoff_millis;
        let end_millis = stats.end_millis;
        let snapshot_millis = stats.snapshot_millis;
        self.graph_loading.insert(mode);

        let tx = tx.clone();
        let config = self.config.clone();
        let generation = self.request_generation;
        thread::spawn(move || {
            let result = db::load_usage_token_buckets_at_scoped(
                &config.db_path,
                mode,
                cutoff_millis,
                end_millis,
                snapshot_millis,
                &config.scope,
                &config.current_directory,
            )
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(RefreshMessage::DashboardGraph {
                generation,
                mode,
                cutoff_millis,
                snapshot_millis,
                result,
            });
        });
    }

    fn enter_calendar(&mut self, tx: &Sender<RefreshMessage>) {
        self.view = View::CalendarOverview;
        self.error = None;
        self.ensure_calendar_costs(tx);
    }

    fn open_calendar_detail(&mut self, tx: &Sender<RefreshMessage>) {
        self.view = View::CalendarDetail;
        self.history_model_scroll = 0;
        self.history_token_bucket = None;
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
        self.history_model_scroll = 0;
        self.history_token_bucket = None;
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
        self.history_model_scroll = 0;
        self.history_token_bucket = None;
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
        self.history_model_scroll = 0;
        self.history_token_bucket = None;
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
        let generation = self.request_generation;
        thread::spawn(move || {
            let result = db::load_period_costs_scoped(
                &config.db_path,
                &periods,
                &config.scope,
                &config.current_directory,
            )
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(RefreshMessage::Calendar { generation, result });
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
        let generation = self.request_generation;
        thread::spawn(move || {
            let result = db::load_usage_summary_between_scoped(
                &config.db_path,
                period.mode(),
                period.start_millis,
                period.end_millis,
                &config.scope,
                &config.current_directory,
            )
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(RefreshMessage::History {
                generation,
                period,
                result,
            });
        });
    }

    fn trigger_history_graph_refresh(&mut self, period: PeriodKey, tx: &Sender<RefreshMessage>) {
        if self.history_graph_loading.contains(&period)
            || !self.history_graph_refresh_pending.contains(&period)
        {
            return;
        }
        let Some(stats) = self.history_stats.get(&period) else {
            return;
        };
        if stats.totals.messages == 0 {
            self.history_graph_refresh_pending.remove(&period);
            return;
        }

        let snapshot_millis = stats.snapshot_millis;
        self.history_graph_loading.insert(period);

        let tx = tx.clone();
        let config = self.config.clone();
        let generation = self.request_generation;
        thread::spawn(move || {
            let result = db::load_usage_token_buckets_at_scoped(
                &config.db_path,
                period.mode(),
                Some(period.start_millis),
                Some(period.end_millis),
                snapshot_millis,
                &config.scope,
                &config.current_directory,
            )
            .map_err(|error| format!("{error:#}"));
            let _ = tx.send(RefreshMessage::HistoryGraph {
                generation,
                period,
                snapshot_millis,
                result,
            });
        });
    }

    fn apply_dashboard_refresh(
        &mut self,
        mode: Mode,
        result: std::result::Result<UsageStats, String>,
    ) {
        self.loading.remove(&mode);
        match result {
            Ok(mut stats) => {
                let mut retained_graph = false;
                if let Some(previous) = self.stats.get(&mode).filter(|previous| {
                    previous.cutoff_millis == stats.cutoff_millis
                        && previous.end_millis == stats.end_millis
                        && stats.totals.messages > 0
                }) {
                    stats.token_buckets.clone_from(&previous.token_buckets);
                    retained_graph = !stats.token_buckets.is_empty();
                }
                if stats.totals.messages > 0 {
                    self.graph_refresh_pending.insert(mode);
                } else {
                    self.graph_refresh_pending.remove(&mode);
                }
                self.stats.insert(mode, stats);
                if self.view == View::Dashboard && mode == self.mode {
                    if !retained_graph {
                        self.dashboard_token_bucket = None;
                    }
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

    fn apply_dashboard_graph_refresh(
        &mut self,
        mode: Mode,
        cutoff_millis: Option<i64>,
        snapshot_millis: i64,
        result: std::result::Result<Vec<db::TokenBucket>, String>,
    ) -> bool {
        let placeholder_was_visible = self.view == View::Dashboard
            && mode == self.mode
            && self
                .stats
                .get(&mode)
                .map(|stats| stats.token_buckets.is_empty())
                .unwrap_or(false);
        self.graph_loading.remove(&mode);
        let is_current_request = self
            .stats
            .get(&mode)
            .map(|stats| {
                stats.cutoff_millis == cutoff_millis && stats.snapshot_millis == snapshot_millis
            })
            .unwrap_or(false);
        if !is_current_request {
            return false;
        }
        self.graph_refresh_pending.remove(&mode);
        match result {
            Ok(token_buckets) => {
                let mut graph_changed = false;
                if let Some(stats) = self.stats.get_mut(&mode) {
                    if stats.token_buckets != token_buckets {
                        graph_changed = true;
                        stats.token_buckets = token_buckets;
                        if self
                            .dashboard_token_bucket
                            .map(|idx| idx >= stats.token_buckets.len())
                            .unwrap_or(false)
                        {
                            self.dashboard_token_bucket = None;
                        }
                    }
                }
                let mut error_changed = false;
                if self.view == View::Dashboard && mode == self.mode {
                    error_changed = self.error.is_some();
                    self.error = None;
                }
                (self.view == View::Dashboard && mode == self.mode)
                    && (placeholder_was_visible || graph_changed || error_changed)
            }
            Err(error) => {
                if self.view == View::Dashboard && mode == self.mode {
                    let changed = self.error.as_deref() != Some(error.as_str());
                    self.error = Some(error);
                    changed
                } else {
                    false
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
            Ok(mut stats) => {
                let mut retained_graph = false;
                if let Some(previous) = self.history_stats.get(&period).filter(|previous| {
                    previous.cutoff_millis == stats.cutoff_millis
                        && previous.end_millis == stats.end_millis
                        && stats.totals.messages > 0
                }) {
                    stats.token_buckets.clone_from(&previous.token_buckets);
                    retained_graph = !stats.token_buckets.is_empty();
                }
                if stats.totals.messages > 0 {
                    self.history_graph_refresh_pending.insert(period);
                } else {
                    self.history_graph_refresh_pending.remove(&period);
                }
                self.history_stats.insert(period, stats);
                if self.view == View::CalendarDetail && period == self.calendar.selected {
                    if !retained_graph {
                        self.history_token_bucket = None;
                    }
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

    fn apply_history_graph_refresh(
        &mut self,
        period: PeriodKey,
        snapshot_millis: i64,
        result: std::result::Result<Vec<db::TokenBucket>, String>,
    ) -> bool {
        let placeholder_was_visible = self.view == View::CalendarDetail
            && period == self.calendar.selected
            && self
                .history_stats
                .get(&period)
                .map(|stats| stats.token_buckets.is_empty())
                .unwrap_or(false);
        self.history_graph_loading.remove(&period);
        let is_current_request = self
            .history_stats
            .get(&period)
            .map(|stats| stats.snapshot_millis == snapshot_millis)
            .unwrap_or(false);
        if !is_current_request {
            return false;
        }
        self.history_graph_refresh_pending.remove(&period);
        match result {
            Ok(token_buckets) => {
                let mut graph_changed = false;
                if let Some(stats) = self.history_stats.get_mut(&period) {
                    if stats.token_buckets != token_buckets {
                        graph_changed = true;
                        stats.token_buckets = token_buckets;
                        if self
                            .history_token_bucket
                            .map(|idx| idx >= stats.token_buckets.len())
                            .unwrap_or(false)
                        {
                            self.history_token_bucket = None;
                        }
                    }
                }
                let mut error_changed = false;
                if self.view == View::CalendarDetail && period == self.calendar.selected {
                    error_changed = self.error.is_some();
                    self.error = None;
                }
                (self.view == View::CalendarDetail && period == self.calendar.selected)
                    && (placeholder_was_visible || graph_changed || error_changed)
            }
            Err(error) => {
                if self.view == View::CalendarDetail && period == self.calendar.selected {
                    let changed = self.error.as_deref() != Some(error.as_str());
                    self.error = Some(error);
                    changed
                } else {
                    false
                }
            }
        }
    }

    fn ensure_visible_graph(&mut self, area: Rect, tx: &Sender<RefreshMessage>) -> bool {
        if tui::token_graph_capacity_area(area, self).is_none() {
            return false;
        }

        match self.view {
            View::Dashboard => {
                let was_loading = self.graph_loading.contains(&self.mode);
                self.trigger_dashboard_graph_refresh(self.mode, tx);
                !was_loading
                    && self.graph_loading.contains(&self.mode)
                    && self
                        .current_stats()
                        .map(|stats| stats.token_buckets.is_empty())
                        .unwrap_or(false)
            }
            View::CalendarDetail => {
                let period = self.calendar.selected;
                let was_loading = self.history_graph_loading.contains(&period);
                self.trigger_history_graph_refresh(period, tx);
                !was_loading
                    && self.history_graph_loading.contains(&period)
                    && self
                        .selected_history_stats()
                        .map(|stats| stats.token_buckets.is_empty())
                        .unwrap_or(false)
            }
            View::CalendarOverview => false,
        }
    }

    fn prefetch_dashboard_summaries(&mut self, tx: &Sender<RefreshMessage>) {
        if self.view != View::Dashboard
            || !self.stats.contains_key(&self.mode)
            || !self.loading.is_empty()
            || !self.graph_loading.is_empty()
            || self.calendar_loading
            || !self.history_loading.is_empty()
            || !self.history_graph_loading.is_empty()
        {
            return;
        }

        for mode in Mode::ALL {
            if mode != self.mode
                && !self.stats.contains_key(&mode)
                && !self.dashboard_prefetch_attempted.contains(&mode)
            {
                self.dashboard_prefetch_attempted.insert(mode);
                self.trigger_dashboard_refresh_for_mode(mode, tx, false);
                break;
            }
        }
    }

    fn move_help_selection(&mut self, steps: i32, layout: &tui::HelpLayoutState) {
        for _ in 0..steps.unsigned_abs() {
            if steps > 0 {
                self.move_help_down(layout);
            } else {
                self.move_help_up(layout);
            }
        }
    }

    fn move_model_breakdown_scroll(&mut self, steps: i32, area: Rect) {
        let max_scroll = match self.view {
            View::Dashboard => self
                .current_stats()
                .map(|stats| tui::model_breakdown_max_scroll(area, stats.models.len())),
            View::CalendarDetail => self
                .selected_history_stats()
                .map(|stats| tui::model_breakdown_max_scroll(area, stats.models.len())),
            View::CalendarOverview => None,
        }
        .unwrap_or(0);
        let amount = steps.unsigned_abs() as usize;

        match self.view {
            View::Dashboard => {
                let current = self.dashboard_model_scroll.min(max_scroll);
                self.dashboard_model_scroll = if steps > 0 {
                    current.saturating_add(amount).min(max_scroll)
                } else {
                    current.saturating_sub(amount)
                };
            }
            View::CalendarDetail => {
                let current = self.history_model_scroll.min(max_scroll);
                self.history_model_scroll = if steps > 0 {
                    current.saturating_add(amount).min(max_scroll)
                } else {
                    current.saturating_sub(amount)
                };
            }
            View::CalendarOverview => {}
        }
    }

    fn select_token_bucket(&mut self, bucket_idx: usize) {
        match self.view {
            View::Dashboard => self.dashboard_token_bucket = Some(bucket_idx),
            View::CalendarDetail => self.history_token_bucket = Some(bucket_idx),
            View::CalendarOverview => {}
        }
    }

    fn move_help_down(&mut self, layout: &tui::HelpLayoutState) {
        if self.help_config_visible(layout) {
            if self.config_selection + 1 < ConfigEditorItem::ALL.len() {
                self.config_selection += 1;
                self.ensure_help_selection_visible(layout);
            } else {
                self.help_scroll = self.help_scroll.saturating_add(1).min(layout.max_scroll);
            }
        } else {
            self.help_scroll = self.help_scroll.saturating_add(1).min(layout.max_scroll);
        }
    }

    fn move_help_up(&mut self, layout: &tui::HelpLayoutState) {
        if self.help_config_visible(layout) && self.config_selection > 0 {
            self.config_selection -= 1;
            self.ensure_help_selection_visible(layout);
        } else {
            self.help_scroll = self.help_scroll.saturating_sub(1);
        }
    }

    fn help_config_visible(&self, layout: &tui::HelpLayoutState) -> bool {
        help_config_visible(self.help_scroll.min(layout.max_scroll), layout)
    }

    fn ensure_help_selection_visible(&mut self, layout: &tui::HelpLayoutState) {
        let Some(&selected_row) = layout.config_item_starts.get(self.config_selection) else {
            return;
        };

        let visible_start = self.help_scroll.min(layout.max_scroll);
        let visible_end = visible_start.saturating_add(layout.visible_height);
        if selected_row < visible_start {
            self.help_scroll = selected_row.min(layout.max_scroll);
        } else if selected_row >= visible_end {
            self.help_scroll = selected_row
                .saturating_add(1)
                .saturating_sub(layout.visible_height)
                .min(layout.max_scroll);
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
            ConfigEditorItem::DailyStart => {
                self.config.daily_start = shift_daily_start(self.config.daily_start, direction);
                self.apply_time_window_config_change(tx)?;
            }
            ConfigEditorItem::RefreshSeconds => {
                self.config.refresh_interval =
                    shift_refresh_interval(self.config.refresh_interval, direction);
                self.next_refresh_due = Instant::now() + self.config.refresh_interval;
            }
            ConfigEditorItem::WeekStart => {
                self.config.week_start = cycle_week_start(self.config.week_start, direction);
                self.apply_time_window_config_change(tx)?;
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

    fn apply_time_window_config_change(&mut self, tx: &Sender<RefreshMessage>) -> Result<()> {
        match self.view {
            View::Dashboard => self.trigger_dashboard_refresh(tx),
            View::CalendarOverview | View::CalendarDetail => {
                self.realign_calendar_for_config(tx)?
            }
        }
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
        self.history_model_scroll = 0;
        self.history_token_bucket = None;
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
        generation: u64,
        mode: Mode,
        result: std::result::Result<UsageStats, String>,
    },
    DashboardGraph {
        generation: u64,
        mode: Mode,
        cutoff_millis: Option<i64>,
        snapshot_millis: i64,
        result: std::result::Result<Vec<db::TokenBucket>, String>,
    },
    Calendar {
        generation: u64,
        result: std::result::Result<Vec<db::PeriodCost>, String>,
    },
    History {
        generation: u64,
        period: PeriodKey,
        result: std::result::Result<UsageStats, String>,
    },
    HistoryGraph {
        generation: u64,
        period: PeriodKey,
        snapshot_millis: i64,
        result: std::result::Result<Vec<db::TokenBucket>, String>,
    },
}

impl RefreshMessage {
    fn generation(&self) -> u64 {
        match self {
            Self::Dashboard { generation, .. }
            | Self::DashboardGraph { generation, .. }
            | Self::Calendar { generation, .. }
            | Self::History { generation, .. }
            | Self::HistoryGraph { generation, .. } => *generation,
        }
    }
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
    let mut needs_draw = true;
    let mut last_size = None;

    loop {
        needs_draw |= drain_refreshes(&rx, &mut app, &tx);
        needs_draw |= maybe_auto_refresh(&mut app, &tx);
        let size = terminal.size()?;
        let current_size = (size.width, size.height);
        if last_size != Some(current_size) {
            last_size = Some(current_size);
            needs_draw = true;
        }
        let area = Rect::new(0, 0, size.width, size.height);
        needs_draw |= app.ensure_visible_graph(area, &tx);
        app.prefetch_dashboard_summaries(&tx);

        if needs_draw {
            terminal.draw(|frame| tui::draw(frame, &app))?;
            needs_draw = false;
        }

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let size = terminal.size()?;
                    let area = Rect::new(0, 0, size.width, size.height);
                    if handle_key(key.code, key.modifiers, area, &mut app, &tx) {
                        return Ok(());
                    }
                    needs_draw = true;
                }
                Event::Mouse(mouse) => {
                    let size = terminal.size()?;
                    let area = Rect::new(0, 0, size.width, size.height);
                    needs_draw |= handle_mouse(mouse, area, &mut app, &tx);
                }
                Event::Resize(_, _) => needs_draw = true,
                _ => {}
            }
        }
    }
}

fn handle_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    area: Rect,
    app: &mut AppState,
    tx: &Sender<RefreshMessage>,
) -> bool {
    if app.scope_picker.is_some() {
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Esc | KeyCode::Char('p') => app.scope_picker = None,
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => app.move_scope_selection(-1),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => app.move_scope_selection(1),
            KeyCode::Enter | KeyCode::Char(' ') => app.apply_scope_selection(tx),
            _ => {}
        }
        return false;
    }

    if code == KeyCode::Char('?') {
        app.toggle_help();
        return false;
    }

    if app.show_help {
        let help_layout = tui::help_layout_state(area, app);
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return true,
            KeyCode::Up | KeyCode::Char('k') => {
                app.move_help_selection(-1, &help_layout);
                return false;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                app.move_help_selection(1, &help_layout);
                return false;
            }
            KeyCode::BackTab => {
                app.move_help_selection(-1, &help_layout);
                return false;
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if app.help_config_visible(&help_layout) {
                    apply_config_action(app, tx, |app, tx| app.edit_selected_config(-1, tx));
                }
                return false;
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter | KeyCode::Char(' ') => {
                if app.help_config_visible(&help_layout) {
                    apply_config_action(app, tx, |app, tx| app.edit_selected_config(1, tx));
                }
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
        KeyCode::Char('p') => {
            app.open_scope_picker();
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

fn handle_mouse(
    mouse: MouseEvent,
    area: Rect,
    app: &mut AppState,
    tx: &Sender<RefreshMessage>,
) -> bool {
    if app.scope_picker.is_some() {
        return false;
    }

    if app.show_help {
        let help_layout = tui::help_layout_state(area, app);
        return match mouse.kind {
            MouseEventKind::ScrollUp => {
                app.move_help_selection(-1, &help_layout);
                true
            }
            MouseEventKind::ScrollDown => {
                app.move_help_selection(1, &help_layout);
                true
            }
            _ => false,
        };
    }

    let mut changed = false;
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            changed = app.token_graph_dragging;
            app.token_graph_dragging = false;
            if let Some(bucket_idx) =
                tui::token_bucket_at_position(mouse.column, mouse.row, area, app)
            {
                app.select_token_bucket(bucket_idx);
                app.token_graph_dragging = true;
                return true;
            }
        }
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved
            if app.token_graph_dragging =>
        {
            if let Some(bucket_idx) =
                tui::token_bucket_at_position(mouse.column, mouse.row, area, app)
            {
                let previous = match app.view {
                    View::Dashboard => app.dashboard_token_bucket,
                    View::CalendarDetail => app.history_token_bucket,
                    View::CalendarOverview => None,
                };
                app.select_token_bucket(bucket_idx);
                return previous != Some(bucket_idx);
            }
            return false;
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let changed = app.token_graph_dragging;
            app.token_graph_dragging = false;
            return changed;
        }
        _ => {}
    }

    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            if tui::model_breakdown_at_position(mouse.column, mouse.row, area, app) =>
        {
            if let Some(model_area) = tui::model_breakdown_area(area, app) {
                let previous = (app.dashboard_model_scroll, app.history_model_scroll);
                let steps = if matches!(mouse.kind, MouseEventKind::ScrollDown) {
                    1
                } else {
                    -1
                };
                app.move_model_breakdown_scroll(steps, model_area);
                return previous != (app.dashboard_model_scroll, app.history_model_scroll);
            }
            return false;
        }
        _ => {}
    }

    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return changed;
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
        return true;
    }

    if let Some(period) = tui::calendar_period_at_position(mouse.column, mouse.row, area, app) {
        apply_calendar_action(app, tx, |app, tx| {
            app.select_calendar_period(period, tx)?;
            app.open_calendar_detail(tx);
            Ok(())
        });
        return true;
    }

    changed
}

fn drain_refreshes(
    rx: &Receiver<RefreshMessage>,
    app: &mut AppState,
    tx: &Sender<RefreshMessage>,
) -> bool {
    let mut needs_draw = false;
    while let Ok(message) = rx.try_recv() {
        if message.generation() != app.request_generation {
            continue;
        }
        match message {
            RefreshMessage::Dashboard {
                generation: _,
                mode,
                result,
            } => {
                app.apply_dashboard_refresh(mode, result);
                needs_draw = true;
            }
            RefreshMessage::DashboardGraph {
                generation: _,
                mode,
                cutoff_millis,
                snapshot_millis,
                result,
            } => {
                needs_draw |=
                    app.apply_dashboard_graph_refresh(mode, cutoff_millis, snapshot_millis, result);
            }
            RefreshMessage::Calendar {
                generation: _,
                result,
            } => {
                app.apply_calendar_refresh(result, tx);
                needs_draw = true;
            }
            RefreshMessage::History {
                generation: _,
                period,
                result,
            } => {
                app.apply_history_refresh(period, result);
                needs_draw = true;
            }
            RefreshMessage::HistoryGraph {
                generation: _,
                period,
                snapshot_millis,
                result,
            } => {
                needs_draw |= app.apply_history_graph_refresh(period, snapshot_millis, result);
            }
        }
    }
    needs_draw
}

fn maybe_auto_refresh(app: &mut AppState, tx: &Sender<RefreshMessage>) -> bool {
    if !app.config.auto_refresh || Instant::now() < app.next_refresh_due {
        return false;
    }

    app.trigger_current_refresh(tx);
    app.next_refresh_due = Instant::now() + app.config.refresh_interval;
    true
}

fn refresh_dashboard_summary(config: Config, mode: Mode) -> Result<UsageStats> {
    let cutoff_millis =
        time_window::cutoff_millis(mode, Local::now(), config.daily_start, config.week_start)?;
    db::load_usage_summary_scoped(
        &config.db_path,
        mode,
        cutoff_millis,
        &config.scope,
        &config.current_directory,
    )
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

fn help_config_visible(scroll: usize, layout: &tui::HelpLayoutState) -> bool {
    layout.visible_height > 0
        && layout.config_end > layout.config_start
        && scroll.saturating_add(layout.visible_height) > layout.config_start
        && scroll < layout.config_end
}

fn cycle_week_start(current: WeekStart, direction: i32) -> WeekStart {
    cycle_value(&[WeekStart::Monday, WeekStart::Sunday], current, direction)
}

fn shift_daily_start(current: DailyStart, direction: i32) -> DailyStart {
    const DAY_MINUTES: i32 = 24 * 60;
    const STEP_MINUTES: i32 = 15;

    let current_minutes = (current.hour * 60 + current.minute) as i32;
    let shifted = (current_minutes + direction.signum() * STEP_MINUTES).rem_euclid(DAY_MINUTES);

    DailyStart {
        hour: (shifted / 60) as u32,
        minute: (shifted % 60) as u32,
    }
}

fn shift_refresh_interval(current: Duration, direction: i32) -> Duration {
    const STEP_SECONDS: u64 = 15;

    let current = current.as_secs().max(1);
    let next = if direction < 0 {
        current.saturating_sub(STEP_SECONDS).max(1)
    } else {
        current.saturating_add(STEP_SECONDS)
    };

    Duration::from_secs(next)
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
        db::{ModelUsage, TokenBucket, UsageTotals},
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
        let area = Rect::new(0, 0, 120, 40);

        let should_quit = handle_key(KeyCode::Char(' '), KeyModifiers::NONE, area, &mut app, &tx);

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

        assert_eq!(app.selected_config_item(), ConfigEditorItem::DailyStart);

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
    fn idle_mouse_movement_does_not_request_a_redraw() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut app = AppState::new(test_config(tempdir.path().join("config.toml"))).unwrap();
        let (tx, _rx) = mpsc::channel();

        let changed = handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: 10,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 120, 24),
            &mut app,
            &tx,
        );

        assert!(!changed);
    }

    #[test]
    fn small_help_mouse_wheel_scrolls_before_config_selection() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path)).unwrap();
        app.show_help = true;
        let (tx, _rx) = mpsc::channel();
        let area = Rect::new(0, 0, 80, 16);

        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            area,
            &mut app,
            &tx,
        );

        assert_eq!(app.help_scroll, 1);
        assert_eq!(app.selected_config_item(), ConfigEditorItem::AutoRefresh);
    }

    #[test]
    fn help_edit_keys_do_nothing_before_config_is_visible() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path.clone())).unwrap();
        app.show_help = true;
        let (tx, _rx) = mpsc::channel();
        let area = Rect::new(0, 0, 80, 16);

        handle_key(KeyCode::Char(' '), KeyModifiers::NONE, area, &mut app, &tx);
        handle_key(KeyCode::Right, KeyModifiers::NONE, area, &mut app, &tx);

        assert!(app.config.auto_refresh);
        assert!(app.config_notice.is_none());
        assert!(!config_path.exists());
    }

    #[test]
    fn reopening_help_resets_scroll_and_config_selection() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path)).unwrap();
        app.show_help = true;
        app.help_scroll = 8;
        app.config_selection = 4;
        let (tx, _rx) = mpsc::channel();
        let area = Rect::new(0, 0, 80, 16);

        handle_key(KeyCode::Char('?'), KeyModifiers::NONE, area, &mut app, &tx);
        handle_key(KeyCode::Char('?'), KeyModifiers::NONE, area, &mut app, &tx);

        assert!(app.show_help);
        assert_eq!(app.help_scroll, 0);
        assert_eq!(app.selected_config_item(), ConfigEditorItem::AutoRefresh);
    }

    #[test]
    fn help_cycles_selected_option_and_saves_config() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path.clone())).unwrap();
        app.show_help = true;
        let (tx, _rx) = mpsc::channel();
        let area = Rect::new(0, 0, 120, 40);

        for _ in 0..4 {
            handle_key(KeyCode::Down, KeyModifiers::NONE, area, &mut app, &tx);
        }
        handle_key(KeyCode::Right, KeyModifiers::NONE, area, &mut app, &tx);

        assert_eq!(app.selected_config_item(), ConfigEditorItem::ColorTheme);
        assert_eq!(app.config.color_theme, ColorTheme::Ember);
        assert!(fs::read_to_string(config_path)
            .unwrap()
            .contains(r#"color_theme = "ember""#));
    }

    #[test]
    fn help_edits_timing_config_and_saves() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path.clone())).unwrap();
        app.show_help = true;
        let (tx, _rx) = mpsc::channel();
        let area = Rect::new(0, 0, 120, 40);

        handle_key(KeyCode::Down, KeyModifiers::NONE, area, &mut app, &tx);
        handle_key(KeyCode::Right, KeyModifiers::NONE, area, &mut app, &tx);

        assert_eq!(app.selected_config_item(), ConfigEditorItem::DailyStart);
        assert_eq!(
            app.config.daily_start,
            DailyStart {
                hour: 4,
                minute: 15
            }
        );

        handle_key(KeyCode::Down, KeyModifiers::NONE, area, &mut app, &tx);
        handle_key(KeyCode::Left, KeyModifiers::NONE, area, &mut app, &tx);

        assert_eq!(app.selected_config_item(), ConfigEditorItem::RefreshSeconds);
        assert_eq!(app.config.refresh_interval, Duration::from_secs(45));

        let content = fs::read_to_string(config_path).unwrap();
        assert!(content.contains(r#"daily_start = "04:15""#));
        assert!(content.contains("refresh_seconds = 45"));
    }

    #[test]
    fn small_help_scrolls_before_config_selection() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path)).unwrap();
        app.show_help = true;
        let (tx, _rx) = mpsc::channel();
        let area = Rect::new(0, 0, 80, 16);

        handle_key(KeyCode::Down, KeyModifiers::NONE, area, &mut app, &tx);

        assert_eq!(app.help_scroll, 1);
        assert_eq!(app.selected_config_item(), ConfigEditorItem::AutoRefresh);

        loop {
            let layout = tui::help_layout_state(area, &app);
            if app.help_scroll.saturating_add(layout.visible_height) > layout.config_start {
                break;
            }
            handle_key(KeyCode::Down, KeyModifiers::NONE, area, &mut app, &tx);
        }

        assert_eq!(app.selected_config_item(), ConfigEditorItem::AutoRefresh);

        handle_key(KeyCode::Down, KeyModifiers::NONE, area, &mut app, &tx);

        assert_eq!(app.selected_config_item(), ConfigEditorItem::DailyStart);
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

    #[test]
    fn model_breakdown_mouse_wheel_scrolls_dashboard_pane() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path)).unwrap();
        app.stats
            .insert(Mode::Daily, many_model_stats(Mode::Daily, 12));
        let (tx, _rx) = mpsc::channel();
        let area = Rect::new(0, 0, 100, 16);
        let model_area = tui::model_breakdown_area(area, &app).unwrap();

        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: model_area.x + 1,
                row: model_area.y + 1,
                modifiers: KeyModifiers::NONE,
            },
            area,
            &mut app,
            &tx,
        );

        assert_eq!(app.dashboard_model_scroll, 1);

        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: model_area.x + 1,
                row: model_area.y + 1,
                modifiers: KeyModifiers::NONE,
            },
            area,
            &mut app,
            &tx,
        );

        assert_eq!(app.dashboard_model_scroll, 0);
    }

    #[test]
    fn model_breakdown_mouse_wheel_scrolls_history_pane_separately() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path)).unwrap();
        app.view = View::CalendarDetail;
        app.history_stats
            .insert(app.calendar.selected, many_model_stats(Mode::Daily, 12));
        let (tx, _rx) = mpsc::channel();
        let area = Rect::new(0, 0, 100, 16);
        let model_area = tui::model_breakdown_area(area, &app).unwrap();

        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: model_area.x + 1,
                row: model_area.y + 1,
                modifiers: KeyModifiers::NONE,
            },
            area,
            &mut app,
            &tx,
        );

        assert_eq!(app.dashboard_model_scroll, 0);
        assert_eq!(app.history_model_scroll, 1);
    }

    #[test]
    fn token_graph_click_and_drag_selects_dashboard_bucket() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path)).unwrap();
        let bucket_count = 8;
        app.stats
            .insert(Mode::Daily, many_model_stats(Mode::Daily, bucket_count));
        let (tx, _rx) = mpsc::channel();
        let area = Rect::new(0, 0, 100, 32);
        let graph_area = tui::token_graph_area(area, &app).unwrap();
        let inner_width = graph_area.width.saturating_sub(2) as usize;
        let bucket_one_column = graph_area.x + 1 + inner_width.div_ceil(bucket_count) as u16;

        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: bucket_one_column,
                row: graph_area.y + 2,
                modifiers: KeyModifiers::NONE,
            },
            area,
            &mut app,
            &tx,
        );

        assert_eq!(app.dashboard_token_bucket, Some(1));
        assert!(app.token_graph_dragging);

        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: graph_area.x + graph_area.width - 1,
                row: graph_area.y + 2,
                modifiers: KeyModifiers::NONE,
            },
            area,
            &mut app,
            &tx,
        );

        assert_eq!(app.dashboard_token_bucket, Some(7));

        handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: graph_area.x + graph_area.width - 1,
                row: graph_area.y + 2,
                modifiers: KeyModifiers::NONE,
            },
            area,
            &mut app,
            &tx,
        );

        assert!(!app.token_graph_dragging);
    }

    #[test]
    fn initial_dashboard_summary_defers_graph_loading() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut app = AppState::new(test_config(tempdir.path().join("config.toml"))).unwrap();
        let mut summary = many_model_stats(Mode::Daily, 4);
        summary.token_buckets.clear();

        app.apply_dashboard_refresh(Mode::Daily, Ok(summary));

        assert!(app.current_stats().unwrap().token_buckets.is_empty());
        assert!(app.graph_refresh_pending.contains(&Mode::Daily));
    }

    #[test]
    fn dashboard_refresh_retains_graph_until_replacement_arrives() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut app = AppState::new(test_config(tempdir.path().join("config.toml"))).unwrap();
        let previous = many_model_stats(Mode::Daily, 4);
        let previous_buckets = previous.token_buckets.clone();
        let previous_refreshed_at = previous.refreshed_at;
        app.stats.insert(Mode::Daily, previous);
        app.dashboard_token_bucket = Some(2);

        let mut summary = many_model_stats(Mode::Daily, 4);
        summary.refreshed_at = previous_refreshed_at + chrono::Duration::minutes(1);
        summary.token_buckets.clear();
        app.apply_dashboard_refresh(Mode::Daily, Ok(summary));

        assert_eq!(app.current_stats().unwrap().token_buckets, previous_buckets);
        assert_eq!(app.dashboard_token_bucket, Some(2));
        assert!(app.graph_refresh_pending.contains(&Mode::Daily));
    }

    #[test]
    fn stale_dashboard_graph_result_does_not_replace_retained_graph() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut app = AppState::new(test_config(tempdir.path().join("config.toml"))).unwrap();
        let previous = many_model_stats(Mode::Daily, 4);
        let previous_buckets = previous.token_buckets.clone();
        let previous_refreshed_at = previous.refreshed_at;
        let previous_snapshot_millis = previous.snapshot_millis;
        app.stats.insert(Mode::Daily, previous);

        let mut summary = many_model_stats(Mode::Daily, 4);
        summary.refreshed_at = previous_refreshed_at + chrono::Duration::minutes(1);
        summary.snapshot_millis = previous_snapshot_millis + 60_000;
        summary.token_buckets.clear();
        app.apply_dashboard_refresh(Mode::Daily, Ok(summary));
        app.graph_loading.insert(Mode::Daily);

        let replacement = vec![TokenBucket {
            start_millis: 0,
            end_millis: 1,
            tokens: 999,
        }];
        let changed = app.apply_dashboard_graph_refresh(
            Mode::Daily,
            None,
            previous_snapshot_millis,
            Ok(replacement),
        );

        assert!(!changed);
        assert_eq!(app.current_stats().unwrap().token_buckets, previous_buckets);
        assert!(app.graph_refresh_pending.contains(&Mode::Daily));
    }

    #[test]
    fn unchanged_dashboard_graph_does_not_request_a_redraw() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut app = AppState::new(test_config(tempdir.path().join("config.toml"))).unwrap();
        let stats = many_model_stats(Mode::Daily, 4);
        let cutoff_millis = stats.cutoff_millis;
        let snapshot_millis = stats.snapshot_millis;
        let buckets = stats.token_buckets.clone();
        app.stats.insert(Mode::Daily, stats);
        app.graph_loading.insert(Mode::Daily);
        app.graph_refresh_pending.insert(Mode::Daily);

        let changed = app.apply_dashboard_graph_refresh(
            Mode::Daily,
            cutoff_millis,
            snapshot_millis,
            Ok(buckets),
        );

        assert!(!changed);
        assert!(!app.graph_refresh_pending.contains(&Mode::Daily));
    }

    #[test]
    fn history_refresh_retains_graph_until_replacement_arrives() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut app = AppState::new(test_config(tempdir.path().join("config.toml"))).unwrap();
        let period = app.calendar.selected;
        let previous = many_model_stats(period.mode(), 4);
        let previous_buckets = previous.token_buckets.clone();
        let previous_refreshed_at = previous.refreshed_at;
        app.view = View::CalendarDetail;
        app.history_stats.insert(period, previous);
        app.history_token_bucket = Some(2);

        let mut summary = many_model_stats(period.mode(), 4);
        summary.refreshed_at = previous_refreshed_at + chrono::Duration::minutes(1);
        summary.cutoff_millis = Some(period.start_millis);
        summary.end_millis = Some(period.end_millis);
        if let Some(previous) = app.history_stats.get_mut(&period) {
            previous.cutoff_millis = Some(period.start_millis);
            previous.end_millis = Some(period.end_millis);
        }
        summary.token_buckets.clear();
        app.apply_history_refresh(period, Ok(summary));

        assert_eq!(
            app.selected_history_stats().unwrap().token_buckets,
            previous_buckets
        );
        assert_eq!(app.history_token_bucket, Some(2));
        assert!(app.history_graph_refresh_pending.contains(&period));
    }

    #[test]
    fn project_picker_persists_scope_and_invalidates_cached_usage() {
        let tempdir = tempfile::tempdir().unwrap();
        let config_path = tempdir.path().join("config.toml");
        let mut app = AppState::new(test_config(config_path.clone())).unwrap();
        app.projects = vec![db::ProjectInfo {
            id: "project-a".to_string(),
            name: "Project A".to_string(),
            worktree: "/tmp/project".to_string(),
        }];
        app.stats
            .insert(Mode::Daily, many_model_stats(Mode::Daily, 1));
        let (tx, _rx) = mpsc::channel();
        let area = Rect::new(0, 0, 120, 30);

        handle_key(KeyCode::Char('p'), KeyModifiers::NONE, area, &mut app, &tx);
        handle_key(KeyCode::Down, KeyModifiers::NONE, area, &mut app, &tx);
        handle_key(KeyCode::Down, KeyModifiers::NONE, area, &mut app, &tx);
        handle_key(KeyCode::Enter, KeyModifiers::NONE, area, &mut app, &tx);

        assert_eq!(app.config.scope, Scope::Project("project-a".to_string()));
        assert_eq!(app.request_generation, 1);
        assert!(app.stats.is_empty());
        assert!(fs::read_to_string(config_path)
            .unwrap()
            .contains(r#"scope = "project:project-a""#));
    }

    #[test]
    fn ignores_refresh_results_from_an_old_scope_generation() {
        let tempdir = tempfile::tempdir().unwrap();
        let mut app = AppState::new(test_config(tempdir.path().join("config.toml"))).unwrap();
        app.request_generation = 2;
        let (tx, rx) = mpsc::channel();
        tx.send(RefreshMessage::Dashboard {
            generation: 1,
            mode: Mode::Daily,
            result: Ok(many_model_stats(Mode::Daily, 1)),
        })
        .unwrap();

        let changed = drain_refreshes(&rx, &mut app, &tx);

        assert!(!changed);
        assert!(app.stats.is_empty());
    }

    fn test_config(config_path: PathBuf) -> Config {
        Config {
            db_path: PathBuf::from("/tmp/opencode.db"),
            current_directory: PathBuf::from("/tmp/project"),
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

    fn many_model_stats(mode: Mode, count: usize) -> UsageStats {
        let models = (0..count)
            .map(|idx| ModelUsage {
                provider: "provider".to_string(),
                model_id: format!("model-{idx}"),
                variant: "default".to_string(),
                display_name: format!("provider/model-{idx}"),
                totals: UsageTotals {
                    messages: 1,
                    cost: (count - idx) as f64,
                    input: 10,
                    output: 20,
                    cache_read: 30,
                    cache_write: 40,
                },
            })
            .collect::<Vec<_>>();
        let totals = UsageTotals {
            messages: count as u64,
            cost: models.iter().map(|model| model.totals.cost).sum(),
            input: models.iter().map(|model| model.totals.input).sum(),
            output: models.iter().map(|model| model.totals.output).sum(),
            cache_read: models.iter().map(|model| model.totals.cache_read).sum(),
            cache_write: models.iter().map(|model| model.totals.cache_write).sum(),
        };

        UsageStats {
            mode,
            refreshed_at: Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            snapshot_millis: 1_750_000_000_000,
            cutoff_millis: None,
            end_millis: None,
            totals,
            models,
            token_buckets: token_buckets(count),
        }
    }

    fn token_buckets(count: usize) -> Vec<TokenBucket> {
        const HOUR: i64 = 60 * 60 * 1000;
        (0..count)
            .map(|idx| TokenBucket {
                start_millis: idx as i64 * HOUR,
                end_millis: (idx as i64 + 1) * HOUR,
                tokens: (idx as u64 + 1) * 10,
            })
            .collect()
    }
}
