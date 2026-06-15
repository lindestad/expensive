use chrono::{DateTime, Local};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs},
    Frame,
};

use crate::{
    app::AppState,
    db::{ModelUsage, UsageStats, UsageTotals},
    format,
    time_window::Mode,
};

const BORDER: Color = Color::Rgb(64, 74, 92);
const TEXT: Color = Color::Rgb(220, 226, 235);
const MUTED: Color = Color::Rgb(119, 133, 154);
const TITLE: Color = Color::Rgb(170, 220, 255);
const ACCENT: Color = Color::Rgb(80, 210, 190);
const COST: Color = Color::Rgb(255, 196, 118);
const TOKENS: Color = Color::Rgb(155, 210, 255);
const IO: Color = Color::Rgb(187, 162, 255);
const CACHE: Color = Color::Rgb(146, 226, 160);
const ERROR: Color = Color::Rgb(255, 117, 117);

#[derive(Clone, Copy)]
struct MetricStyle {
    value: Color,
    label: Color,
}

pub fn draw(frame: &mut Frame<'_>, app: &AppState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Min(6),
            Constraint::Length(1),
        ])
        .split(area);

    draw_tabs(frame, chunks[0], app);
    draw_summary(
        frame,
        chunks[1],
        app.current_stats(),
        app.is_current_loading(),
    );
    draw_models(
        frame,
        chunks[2],
        app.current_stats(),
        app.is_current_loading(),
    );
    draw_footer(frame, chunks[3], app);
}

pub fn mode_at_tab_position(column: u16, row: u16, area: Rect) -> Option<Mode> {
    if area.width < 3 || area.height < 3 || row != area.y.saturating_add(1) {
        return None;
    }

    let tabs_right = area.x.saturating_add(area.width).saturating_sub(1);
    let mut x = area.x.saturating_add(1);
    for (idx, mode) in Mode::ALL.iter().enumerate() {
        let title_width = mode.title().chars().count() as u16;
        let hit_start = x;
        let hit_end = x
            .saturating_add(title_width)
            .saturating_add(2)
            .min(tabs_right);

        if column >= hit_start && column < hit_end {
            return Some(*mode);
        }

        if idx == Mode::ALL.len() - 1 || hit_end >= tabs_right {
            break;
        }

        x = hit_end.saturating_add(1);
    }

    None
}

fn draw_tabs(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let titles = Mode::ALL
        .iter()
        .map(|mode| Line::from(Span::raw(mode.title())))
        .collect::<Vec<_>>();

    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" expensive ")
                .title_style(Style::default().fg(TITLE).add_modifier(Modifier::BOLD))
                .border_style(Style::default().fg(BORDER)),
        )
        .select(app.mode.index())
        .style(Style::default().fg(MUTED))
        .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
        .divider(" ");

    frame.render_widget(tabs, area);
}

fn draw_summary(frame: &mut Frame<'_>, area: Rect, stats: Option<&UsageStats>, loading: bool) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(area);

    let totals = stats.map(|stats| &stats.totals);
    let cost = totals
        .map(|totals| format::cost(totals.cost))
        .unwrap_or_else(|| "--".to_string());
    let messages = totals
        .map(|totals| format!("{} msgs", format::integer(totals.messages)))
        .unwrap_or_else(|| metric_sub("messages", loading));
    let total_tokens = totals
        .map(|totals| format::tokens(totals.total_tokens()))
        .unwrap_or_else(|| "--".to_string());
    let token_sub = metric_sub("all categories", loading);
    let input_output = totals
        .map(|totals| {
            format!(
                "{} / {}",
                format::tokens(totals.input),
                format::tokens(totals.output)
            )
        })
        .unwrap_or_else(|| "--".to_string());
    let io_sub = metric_sub("input / output", loading);
    let cache = totals
        .map(|totals| {
            format!(
                "{} / {}",
                format::tokens(totals.cache_read),
                format::tokens(totals.cache_write)
            )
        })
        .unwrap_or_else(|| "--".to_string());
    let cache_sub = metric_sub("read / write", loading);

    draw_metric(
        frame,
        chunks[0],
        "Cost",
        &cost,
        &messages,
        MetricStyle {
            value: COST,
            label: MUTED,
        },
    );
    draw_metric(
        frame,
        chunks[1],
        "Total Tokens",
        &total_tokens,
        &token_sub,
        MetricStyle {
            value: TOKENS,
            label: MUTED,
        },
    );
    draw_metric(
        frame,
        chunks[2],
        "Input / Output",
        &input_output,
        &io_sub,
        MetricStyle {
            value: IO,
            label: MUTED,
        },
    );
    draw_metric(
        frame,
        chunks[3],
        "Cache",
        &cache,
        &cache_sub,
        MetricStyle {
            value: CACHE,
            label: MUTED,
        },
    );
}

fn metric_sub(label: &str, loading: bool) -> String {
    if loading {
        "refreshing".to_string()
    } else {
        label.to_string()
    }
}

fn draw_metric(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    value: &str,
    sub: &str,
    style: MetricStyle,
) {
    let paragraph = Paragraph::new(vec![
        Line::from(Span::styled(
            value.to_string(),
            Style::default()
                .fg(style.value)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            sub.to_string(),
            Style::default().fg(style.label),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} "))
            .title_style(Style::default().fg(style.value))
            .border_style(Style::default().fg(BORDER)),
    )
    .alignment(Alignment::Center);

    frame.render_widget(paragraph, area);
}

fn draw_models(frame: &mut Frame<'_>, area: Rect, stats: Option<&UsageStats>, loading: bool) {
    let Some(stats) = stats else {
        let message = if loading {
            "Loading OpenCode usage..."
        } else {
            "No usage loaded. Press r to refresh."
        };
        let paragraph = Paragraph::new(message)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Models ")
                    .title_style(Style::default().fg(TITLE))
                    .border_style(Style::default().fg(BORDER)),
            )
            .style(Style::default().fg(if loading { TOKENS } else { MUTED }))
            .alignment(Alignment::Center);
        frame.render_widget(paragraph, area);
        return;
    };

    if area.width >= 112 {
        draw_wide_models(frame, area, stats);
    } else {
        draw_compact_models(frame, area, stats);
    }
}

fn draw_wide_models(frame: &mut Frame<'_>, area: Rect, stats: &UsageStats) {
    let max_cost = stats.models.first().map(model_cost).unwrap_or(0.0);
    let rows = stats
        .models
        .iter()
        .map(|model| wide_row(model, &stats.totals, max_cost));

    let header = Row::new([
        Cell::from("Model"),
        Cell::from("Msgs"),
        Cell::from("Cost"),
        Cell::from("Tokens"),
        Cell::from("Input"),
        Cell::from("Output"),
        Cell::from("Read"),
        Cell::from("Write"),
        Cell::from("Cost %"),
    ])
    .style(Style::default().fg(TITLE).add_modifier(Modifier::BOLD));

    let table = Table::new(
        rows,
        [
            Constraint::Min(28),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(18),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} by model ", stats.mode.title()))
            .title_style(Style::default().fg(TITLE))
            .border_style(Style::default().fg(BORDER)),
    )
    .column_spacing(1);

    frame.render_widget(table, area);
}

fn draw_compact_models(frame: &mut Frame<'_>, area: Rect, stats: &UsageStats) {
    let max_cost = stats.models.first().map(model_cost).unwrap_or(0.0);
    let rows = stats
        .models
        .iter()
        .map(|model| compact_row(model, &stats.totals, max_cost));

    let header = Row::new([
        Cell::from("Model"),
        Cell::from("Msgs"),
        Cell::from("Cost"),
        Cell::from("Tokens"),
        Cell::from("Cost %"),
    ])
    .style(Style::default().fg(TITLE).add_modifier(Modifier::BOLD));

    let table = Table::new(
        rows,
        [
            Constraint::Min(24),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(18),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} by model ", stats.mode.title()))
            .title_style(Style::default().fg(TITLE))
            .border_style(Style::default().fg(BORDER)),
    )
    .column_spacing(1);

    frame.render_widget(table, area);
}

fn wide_row(model: &ModelUsage, totals: &UsageTotals, max_cost: f64) -> Row<'static> {
    Row::new([
        styled_cell(model.display_name.clone(), ACCENT, true),
        styled_cell(format::integer(model.totals.messages), MUTED, false),
        styled_cell(format::precise_cost(model.totals.cost), COST, true),
        styled_cell(format::tokens(model.totals.total_tokens()), TOKENS, true),
        styled_cell(format::tokens(model.totals.input), IO, false),
        styled_cell(format::tokens(model.totals.output), IO, false),
        styled_cell(format::tokens(model.totals.cache_read), CACHE, false),
        styled_cell(format::tokens(model.totals.cache_write), CACHE, false),
        styled_cell(
            share_cell(model.totals.cost, totals.cost, max_cost),
            COST,
            false,
        ),
    ])
    .style(Style::default().fg(TEXT))
}

fn compact_row(model: &ModelUsage, totals: &UsageTotals, max_cost: f64) -> Row<'static> {
    Row::new([
        styled_cell(model.display_name.clone(), ACCENT, true),
        styled_cell(format::integer(model.totals.messages), MUTED, false),
        styled_cell(format::precise_cost(model.totals.cost), COST, true),
        styled_cell(format::tokens(model.totals.total_tokens()), TOKENS, true),
        styled_cell(
            share_cell(model.totals.cost, totals.cost, max_cost),
            COST,
            false,
        ),
    ])
    .style(Style::default().fg(TEXT))
}

fn styled_cell(value: String, color: Color, bold: bool) -> Cell<'static> {
    let mut style = Style::default().fg(color);
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    Cell::from(value).style(style)
}

fn share_cell(value: f64, total: f64, max: f64) -> String {
    let percentage = format::percent(value, total);
    let bar = braille_bar(value, max, 8);
    format!("{percentage:<6} {bar}")
}

fn braille_bar(value: f64, max: f64, width: usize) -> String {
    if max <= 0.0 {
        return "⠄".repeat(width);
    }

    let filled = ((value / max) * width as f64).round() as usize;
    let filled = filled.clamp(0, width);
    format!("{}{}", "⣿".repeat(filled), "⠄".repeat(width - filled))
}

fn model_cost(model: &ModelUsage) -> f64 {
    model.totals.cost
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let (status, status_color) = if let Some(error) = &app.error {
        (format!("error: {error}"), ERROR)
    } else if let Some(stats) = app.current_stats() {
        let cutoff = cutoff_label(stats);
        (
            format!("{} | {cutoff}", format::timestamp(stats.refreshed_at)),
            MUTED,
        )
    } else if app.is_current_loading() {
        ("loading".to_string(), TOKENS)
    } else {
        ("idle".to_string(), MUTED)
    };

    let text = Line::from(vec![
        Span::styled(
            " Tab ",
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" mode ", Style::default().fg(MUTED)),
        key_span(" S-Tab "),
        Span::styled(" back ", Style::default().fg(MUTED)),
        key_span(" r "),
        Span::styled(" refresh ", Style::default().fg(MUTED)),
        key_span(" q "),
        Span::styled(" quit | ", Style::default().fg(MUTED)),
        Span::styled(status, Style::default().fg(status_color)),
    ]);

    frame.render_widget(Paragraph::new(text).style(Style::default().fg(MUTED)), area);
}

fn key_span(label: &'static str) -> Span<'static> {
    Span::styled(
        label,
        Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(156, 168, 185)),
    )
}

fn cutoff_label(stats: &UsageStats) -> String {
    let Some(cutoff_millis) = stats.cutoff_millis else {
        return "all time".to_string();
    };

    DateTime::from_timestamp_millis(cutoff_millis)
        .map(|cutoff| {
            cutoff
                .with_timezone(&Local)
                .format("since %b %d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "since cutoff".to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        path::PathBuf,
        time::{Duration, Instant},
    };

    use chrono::TimeZone;
    use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};

    use super::*;
    use crate::{
        config::{Config, Scope},
        time_window::DailyStart,
    };

    #[test]
    fn renders_loaded_daily_summary_and_model_breakdown() {
        let stats = sample_stats(Mode::Daily, Some(local_millis(2026, 6, 15, 4, 0, 0)));
        let app = app_with_stats(Mode::Daily, stats);

        let output = render(&app, 120, 24);

        assert!(output.contains("Daily"));
        assert!(output.contains("Weekly"));
        assert!(output.contains("$3.75"));
        assert!(output.contains("2 msgs"));
        assert!(output.contains("110"));
        assert!(output.contains("11 / 22"));
        assert!(output.contains("33 / 44"));
        assert!(output.contains("Daily by model"));
        assert!(output.contains("provider/gpt-test (high)"));
        assert!(output.contains("$2.5000"));
        assert!(output.contains("66.7%  ⣿⣿⣿⣿⣿⣿⣿⣿"));
        assert!(output.contains("since Jun 15 04:00"));
    }

    #[test]
    fn renders_percent_before_bar_in_compact_width() {
        let stats = sample_stats(Mode::Daily, Some(local_millis(2026, 6, 15, 4, 0, 0)));
        let app = app_with_stats(Mode::Daily, stats);

        let output = render(&app, 80, 24);

        assert!(output.contains("Cost %"));
        assert!(output.contains("66.7%"));
        assert!(output.contains("33.3%"));
        assert!(output.contains("⣿"));
        assert!(!output.contains("#"));
    }

    #[test]
    fn aligns_cost_percent_bars_after_variable_width_percentages() {
        let share = share_cell(1.4, 100.0, 98.6);
        let larger_share = share_cell(98.6, 100.0, 98.6);

        assert_eq!(share.find('⠄'), larger_share.find('⣿'));
        assert_eq!(share, "1.4%   ⠄⠄⠄⠄⠄⠄⠄⠄");
        assert_eq!(larger_share, "98.6%  ⣿⣿⣿⣿⣿⣿⣿⣿");
    }

    #[test]
    fn maps_tab_click_positions_to_modes() {
        let area = Rect::new(0, 0, 100, 24);

        assert_eq!(mode_at_tab_position(2, 1, area), Some(Mode::Daily));
        assert_eq!(mode_at_tab_position(10, 1, area), Some(Mode::Weekly));
        assert_eq!(mode_at_tab_position(19, 1, area), Some(Mode::Monthly));
        assert_eq!(mode_at_tab_position(30, 1, area), Some(Mode::AllTime));
    }

    #[test]
    fn ignores_clicks_outside_tab_labels() {
        let area = Rect::new(0, 0, 100, 24);

        assert_eq!(mode_at_tab_position(2, 0, area), None);
        assert_eq!(mode_at_tab_position(8, 1, area), None);
        assert_eq!(mode_at_tab_position(80, 1, area), None);
        assert_eq!(mode_at_tab_position(2, 2, area), None);
    }

    #[test]
    fn renders_all_time_footer_without_cutoff() {
        let stats = sample_stats(Mode::AllTime, None);
        let app = app_with_stats(Mode::AllTime, stats);

        let output = render(&app, 100, 24);

        assert!(output.contains("All Time"));
        assert!(output.contains("All Time by model"));
        assert!(output.contains("all time"));
    }

    #[test]
    fn renders_loading_state_when_current_mode_is_refreshing() {
        let app = app_loading(Mode::Weekly);

        let output = render(&app, 100, 24);

        assert!(output.contains("Weekly"));
        assert!(output.contains("refreshing"));
        assert!(output.contains("Loading OpenCode usage"));
        assert!(output.contains("loading"));
    }

    #[test]
    fn renders_error_footer() {
        let mut app = app_loading(Mode::Monthly);
        app.loading.clear();
        app.error = Some("database is locked".to_string());

        let output = render(&app, 100, 24);

        assert!(output.contains("Monthly"));
        assert!(output.contains("error: database is locked"));
    }

    fn render(app: &AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        buffer_text(terminal.backend().buffer())
    }

    fn buffer_text(buffer: &Buffer) -> String {
        let mut output = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        output
    }

    fn app_with_stats(mode: Mode, stats: UsageStats) -> AppState {
        let mut stats_by_mode = HashMap::new();
        stats_by_mode.insert(mode, stats);

        AppState {
            config: test_config(),
            mode,
            stats: stats_by_mode,
            loading: HashSet::new(),
            error: None,
            last_refresh_started: None,
            next_refresh_due: Instant::now() + Duration::from_secs(60),
        }
    }

    fn app_loading(mode: Mode) -> AppState {
        AppState {
            config: test_config(),
            mode,
            stats: HashMap::new(),
            loading: HashSet::from([mode]),
            error: None,
            last_refresh_started: None,
            next_refresh_due: Instant::now() + Duration::from_secs(60),
        }
    }

    fn test_config() -> Config {
        Config {
            db_path: PathBuf::from("/tmp/opencode.db"),
            daily_start: DailyStart::default(),
            refresh_interval: Duration::from_secs(60),
            auto_refresh: true,
            scope: Scope::All,
        }
    }

    fn sample_stats(mode: Mode, cutoff_millis: Option<i64>) -> UsageStats {
        let high = ModelUsage {
            provider: "provider".to_string(),
            model_id: "gpt-test".to_string(),
            variant: "high".to_string(),
            display_name: "provider/gpt-test (high)".to_string(),
            totals: UsageTotals {
                messages: 1,
                cost: 2.5,
                input: 1,
                output: 2,
                cache_read: 3,
                cache_write: 4,
            },
        };
        let default = ModelUsage {
            provider: "provider".to_string(),
            model_id: "gpt-test".to_string(),
            variant: "default".to_string(),
            display_name: "provider/gpt-test".to_string(),
            totals: UsageTotals {
                messages: 1,
                cost: 1.25,
                input: 10,
                output: 20,
                cache_read: 30,
                cache_write: 40,
            },
        };

        UsageStats {
            mode,
            refreshed_at: Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            cutoff_millis,
            totals: UsageTotals {
                messages: 2,
                cost: 3.75,
                input: 11,
                output: 22,
                cache_read: 33,
                cache_write: 44,
            },
            models: vec![high, default],
        }
    }

    fn local_millis(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, second)
            .unwrap()
            .timestamp_millis()
    }
}
