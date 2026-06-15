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
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .select(app.mode.index())
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
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

    draw_metric(frame, chunks[0], "Cost", &cost, &messages);
    draw_metric(frame, chunks[1], "Total Tokens", &total_tokens, &token_sub);
    draw_metric(frame, chunks[2], "Input / Output", &input_output, &io_sub);
    draw_metric(frame, chunks[3], "Cache", &cache, &cache_sub);
}

fn metric_sub(label: &str, loading: bool) -> String {
    if loading {
        "refreshing".to_string()
    } else {
        label.to_string()
    }
}

fn draw_metric(frame: &mut Frame<'_>, area: Rect, title: &str, value: &str, sub: &str) {
    let paragraph = Paragraph::new(vec![
        Line::from(Span::styled(
            value.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            sub.to_string(),
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} "))
            .border_style(Style::default().fg(Color::DarkGray)),
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
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
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
    .style(
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    );

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
            .border_style(Style::default().fg(Color::DarkGray)),
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
    .style(
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    );

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
            .border_style(Style::default().fg(Color::DarkGray)),
    )
    .column_spacing(1);

    frame.render_widget(table, area);
}

fn wide_row(model: &ModelUsage, totals: &UsageTotals, max_cost: f64) -> Row<'static> {
    Row::new([
        Cell::from(model.display_name.clone()),
        Cell::from(format::integer(model.totals.messages)),
        Cell::from(format::precise_cost(model.totals.cost)),
        Cell::from(format::tokens(model.totals.total_tokens())),
        Cell::from(format::tokens(model.totals.input)),
        Cell::from(format::tokens(model.totals.output)),
        Cell::from(format::tokens(model.totals.cache_read)),
        Cell::from(format::tokens(model.totals.cache_write)),
        Cell::from(share_cell(model.totals.cost, totals.cost, max_cost)),
    ])
    .style(Style::default().fg(Color::White))
}

fn compact_row(model: &ModelUsage, totals: &UsageTotals, max_cost: f64) -> Row<'static> {
    Row::new([
        Cell::from(model.display_name.clone()),
        Cell::from(format::integer(model.totals.messages)),
        Cell::from(format::precise_cost(model.totals.cost)),
        Cell::from(format::tokens(model.totals.total_tokens())),
        Cell::from(share_cell(model.totals.cost, totals.cost, max_cost)),
    ])
    .style(Style::default().fg(Color::White))
}

fn share_cell(value: f64, total: f64, max: f64) -> String {
    let bar = ascii_bar(value, max, 8);
    format!("{bar} {}", format::percent(value, total))
}

fn ascii_bar(value: f64, max: f64, width: usize) -> String {
    if max <= 0.0 {
        return "-".repeat(width);
    }

    let filled = ((value / max) * width as f64).round() as usize;
    let filled = filled.clamp(0, width);
    format!("{}{}", "#".repeat(filled), "-".repeat(width - filled))
}

fn model_cost(model: &ModelUsage) -> f64 {
    model.totals.cost
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let status = if let Some(error) = &app.error {
        format!("error: {error}")
    } else if let Some(stats) = app.current_stats() {
        let cutoff = cutoff_label(stats);
        format!("{} | {cutoff}", format::timestamp(stats.refreshed_at))
    } else if app.is_current_loading() {
        "loading".to_string()
    } else {
        "idle".to_string()
    };

    let text = Line::from(vec![
        Span::styled(
            " Tab ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" mode "),
        Span::styled(" S-Tab ", Style::default().fg(Color::Black).bg(Color::Gray)),
        Span::raw(" back "),
        Span::styled(" r ", Style::default().fg(Color::Black).bg(Color::Gray)),
        Span::raw(" refresh "),
        Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::Gray)),
        Span::raw(format!(" quit | {status}")),
    ]);

    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::Gray)),
        area,
    );
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
        assert!(output.contains("######## 66.7%"));
        assert!(output.contains("since Jun 15 04:00"));
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
