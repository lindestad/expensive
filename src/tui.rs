use chrono::{DateTime, Datelike, Local};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::{
    app::{AppState, View},
    db::{ModelUsage, UsageStats, UsageTotals},
    format,
    time_window::{CalendarScale, Mode, PeriodKey},
};

const BORDER: Color = Color::Rgb(64, 74, 92);
const TEXT: Color = Color::Rgb(220, 226, 235);
const MUTED: Color = Color::Rgb(119, 133, 154);
const TITLE: Color = Color::Rgb(170, 220, 255);
const ACCENT: Color = Color::Rgb(80, 210, 190);
const CALENDAR_ACCENT: Color = Color::Rgb(218, 170, 255);
const COST: Color = Color::Rgb(255, 196, 118);
const TOKENS: Color = Color::Rgb(155, 210, 255);
const IO: Color = Color::Rgb(187, 162, 255);
const CACHE: Color = Color::Rgb(146, 226, 160);
const ERROR: Color = Color::Rgb(255, 117, 117);
const HEAT_PRIMARY_BG: [u8; 7] = [23, 30, 66, 101, 136, 172, 208];
const HEAT_DIM_BG: [u8; 7] = [235, 236, 237, 238, 239, 240, 241];

#[derive(Clone, Copy)]
struct MetricStyle {
    value: Color,
    label: Color,
}

struct FramedGrid<'a> {
    periods: &'a [PeriodKey],
    columns: usize,
    rows: usize,
    selected: PeriodKey,
    max_cost: f64,
}

struct FramedCell {
    period: PeriodKey,
    cost: Option<f64>,
    max_cost: f64,
    selected: bool,
    in_primary_range: bool,
}

#[derive(Clone, Copy)]
enum TabStyle {
    Normal,
    Dashboard,
    Calendar,
}

pub fn draw(frame: &mut Frame<'_>, app: &AppState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(11),
            Constraint::Length(1),
        ])
        .split(area);

    draw_tabs(frame, chunks[0], app);
    match app.view {
        View::Dashboard => draw_stats_view(
            frame,
            chunks[1],
            app.current_stats(),
            app.is_current_loading(),
            app.current_stats()
                .map(|stats| format!("{} by model", stats.mode.title()))
                .unwrap_or_else(|| "Models".to_string()),
        ),
        View::CalendarOverview => draw_calendar_overview(frame, chunks[1], app),
        View::CalendarDetail => draw_stats_view(
            frame,
            chunks[1],
            app.selected_history_stats(),
            app.is_selected_history_loading(),
            format!("{} by model", detail_period_label(app.calendar.selected)),
        ),
    }
    draw_footer(frame, chunks[2], app);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TabTarget {
    Mode(Mode),
    Calendar,
}

pub fn tab_at_position(column: u16, row: u16, area: Rect) -> Option<TabTarget> {
    if area.width < 3 || area.height < 3 || row != area.y.saturating_add(1) {
        return None;
    }

    let mut x = area.x.saturating_add(1);
    for (idx, mode) in Mode::ALL.iter().enumerate() {
        let title_width = tab_width(mode.title());
        let hit_start = x;
        let hit_end = x.saturating_add(title_width);

        if column >= hit_start && column < hit_end {
            return Some(TabTarget::Mode(*mode));
        }

        if idx == Mode::ALL.len() - 1 {
            break;
        }

        x = hit_end.saturating_add(1);
    }

    let calendar_width = tab_width(CALENDAR_TAB);
    let hit_start = area
        .x
        .saturating_add(area.width)
        .saturating_sub(1)
        .saturating_sub(calendar_width);
    let hit_end = hit_start.saturating_add(calendar_width);
    if column >= hit_start && column < hit_end {
        return Some(TabTarget::Calendar);
    }

    None
}

fn draw_tabs(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" expensive ")
        .title_style(Style::default().fg(TITLE).add_modifier(Modifier::BOLD))
        .border_style(Style::default().fg(BORDER));
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    frame.render_widget(block, area);

    let calendar_width = tab_width(CALENDAR_TAB);
    let left_width = inner.width.saturating_sub(calendar_width.saturating_add(1));
    let left_area = Rect::new(inner.x, inner.y, left_width, 1);
    let calendar_area = Rect::new(
        inner.x.saturating_add(left_width),
        inner.y,
        inner.width.saturating_sub(left_width),
        1,
    );

    let mut spans = Vec::new();
    for (idx, mode) in Mode::ALL.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(tab_span(mode.title(), mode_tab_style(app, *mode)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), left_area);
    frame.render_widget(
        Paragraph::new(Line::from(tab_span(
            CALENDAR_TAB,
            if app.view == View::Dashboard {
                TabStyle::Normal
            } else {
                TabStyle::Calendar
            },
        )))
        .alignment(Alignment::Right),
        calendar_area,
    );
}

const CALENDAR_TAB: &str = "Calendar";

fn tab_width(label: &str) -> u16 {
    label.chars().count() as u16 + 2
}

fn mode_tab_style(app: &AppState, mode: Mode) -> TabStyle {
    if app.view == View::Dashboard && app.mode == mode {
        return TabStyle::Dashboard;
    }

    if app.view != View::Dashboard
        && CalendarScale::from_mode(mode)
            .map(|scale| scale == app.calendar.scale)
            .unwrap_or(false)
    {
        return TabStyle::Calendar;
    }

    TabStyle::Normal
}

fn tab_span(label: &str, tab_style: TabStyle) -> Span<'static> {
    let style = match tab_style {
        TabStyle::Normal => Style::default().fg(MUTED),
        TabStyle::Dashboard => Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        TabStyle::Calendar => Style::default()
            .fg(CALENDAR_ACCENT)
            .add_modifier(Modifier::BOLD),
    };
    Span::styled(format!(" {label} "), style)
}

fn draw_stats_view(
    frame: &mut Frame<'_>,
    area: Rect,
    stats: Option<&UsageStats>,
    loading: bool,
    model_title: String,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(6)])
        .split(area);

    draw_summary(frame, chunks[0], stats, loading);
    draw_models(frame, chunks[1], stats, loading, &model_title);
}

fn draw_calendar_overview(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let title = format!(
        " Calendar: {} | {} ",
        app.calendar.scale.title(),
        overview_title(app.calendar.scale, app.calendar.selected)
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(Style::default().fg(TITLE))
        .border_style(Style::default().fg(BORDER));
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    frame.render_widget(block, area);

    match app.calendar.scale {
        CalendarScale::Day => draw_day_calendar(frame, inner, app),
        CalendarScale::Week => draw_period_grid(frame, inner, app, 4),
        CalendarScale::Month => draw_period_grid(frame, inner, app, 3),
    }
}

fn draw_day_calendar(frame: &mut Frame<'_>, area: Rect, app: &AppState) {
    let selected_month = local_date(app.calendar.selected.start_millis)
        .map(|date| (date.year(), date.month()))
        .unwrap_or((0, 0));
    let max_cost = calendar_max_cost(app);

    if can_draw_framed_grid(area, 6, 7, 1) {
        draw_framed_day_calendar(frame, area, app, selected_month, max_cost);
        return;
    }

    let headers = Row::new(["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"])
        .style(Style::default().fg(TITLE).add_modifier(Modifier::BOLD));

    let rows = app.calendar.visible_periods.chunks(7).map(|week| {
        Row::new(week.iter().map(|period| {
            let in_month = local_date(period.start_millis)
                .map(|date| (date.year(), date.month()) == selected_month)
                .unwrap_or(false);
            let cost = app.calendar_cost(*period);
            period_cell(
                *period,
                day_cell_label(*period, cost),
                cost,
                max_cost,
                app.calendar.selected == *period,
                in_month,
            )
        }))
    });

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(14),
            Constraint::Percentage(14),
            Constraint::Percentage(14),
            Constraint::Percentage(14),
            Constraint::Percentage(14),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
        ],
    )
    .header(headers)
    .column_spacing(1);

    frame.render_widget(table, area);
}

fn draw_period_grid(frame: &mut Frame<'_>, area: Rect, app: &AppState, columns: usize) {
    let row_count = app.calendar.visible_periods.len().div_ceil(columns);
    let max_cost = calendar_max_cost(app);

    if can_draw_framed_grid(area, row_count, columns, 0) {
        draw_framed_period_grid(frame, area, app, columns, row_count, max_cost);
        return;
    }

    let constraints = (0..columns)
        .map(|_| Constraint::Ratio(1, columns as u32))
        .collect::<Vec<_>>();
    let rows = app.calendar.visible_periods.chunks(columns).map(|periods| {
        Row::new(periods.iter().map(|period| {
            let cost = app.calendar_cost(*period);
            period_cell(
                *period,
                period_cell_label(*period, cost),
                cost,
                max_cost,
                app.calendar.selected == *period,
                true,
            )
        }))
    });

    let table = Table::new(rows, constraints).column_spacing(1);
    frame.render_widget(table, area);
}

fn draw_framed_day_calendar(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &AppState,
    selected_month: (i32, u32),
    max_cost: f64,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    draw_day_headers(frame, chunks[0]);
    draw_framed_period_rows(
        frame,
        chunks[1],
        app,
        FramedGrid {
            periods: &app.calendar.visible_periods,
            columns: 7,
            rows: 6,
            selected: app.calendar.selected,
            max_cost,
        },
        |period| {
            local_date(period.start_millis)
                .map(|date| (date.year(), date.month()) == selected_month)
                .unwrap_or(false)
        },
        day_framed_lines,
    );
}

fn draw_framed_period_grid(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &AppState,
    columns: usize,
    rows: usize,
    max_cost: f64,
) {
    draw_framed_period_rows(
        frame,
        area,
        app,
        FramedGrid {
            periods: &app.calendar.visible_periods,
            columns,
            rows,
            selected: app.calendar.selected,
            max_cost,
        },
        |_| true,
        period_framed_lines,
    );
}

fn draw_framed_period_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &AppState,
    grid: FramedGrid<'_>,
    is_primary: impl Fn(PeriodKey) -> bool,
    lines: impl Fn(PeriodKey, Option<f64>) -> Vec<Line<'static>>,
) {
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(even_constraints(grid.rows))
        .split(area);

    for row_idx in 0..grid.rows {
        let column_areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(even_constraints(grid.columns))
            .split(row_areas[row_idx]);

        for column_idx in 0..grid.columns {
            let Some(period) = grid
                .periods
                .get(row_idx * grid.columns + column_idx)
                .copied()
            else {
                continue;
            };
            let cost = app.calendar_cost(period);
            let cell = FramedCell {
                period,
                cost,
                max_cost: grid.max_cost,
                selected: grid.selected == period,
                in_primary_range: is_primary(period),
            };
            draw_framed_period_cell(frame, column_areas[column_idx], cell, lines(period, cost));
        }
    }
}

fn draw_day_headers(frame: &mut Frame<'_>, area: Rect) {
    let headers = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(even_constraints(headers.len()))
        .split(area);

    for (idx, header) in headers.iter().enumerate() {
        frame.render_widget(
            Paragraph::new(*header)
                .style(Style::default().fg(TITLE).add_modifier(Modifier::BOLD))
                .alignment(Alignment::Center),
            chunks[idx],
        );
    }
}

fn draw_framed_period_cell(
    frame: &mut Frame<'_>,
    area: Rect,
    cell: FramedCell,
    lines: Vec<Line<'static>>,
) {
    let style = period_style(
        cell.period,
        cell.cost,
        cell.max_cost,
        cell.selected,
        cell.in_primary_range,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(period_border_style(
            style,
            cell.selected,
            cell.in_primary_range,
        ))
        .style(style);
    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(style)
        .alignment(Alignment::Center);
    frame.render_widget(paragraph, area);
}

fn can_draw_framed_grid(area: Rect, rows: usize, columns: usize, extra_rows: u16) -> bool {
    let rows = rows as u16;
    let columns = columns as u16;
    area.height >= rows.saturating_mul(4).saturating_add(extra_rows)
        && area.width >= columns.saturating_mul(11)
}

fn even_constraints(count: usize) -> Vec<Constraint> {
    (0..count)
        .map(|_| Constraint::Ratio(1, count as u32))
        .collect()
}

fn period_border_style(style: Style, selected: bool, in_primary_range: bool) -> Style {
    let color = if selected {
        Color::Black
    } else if in_primary_range {
        BORDER
    } else {
        MUTED
    };
    let mut border = Style::default().fg(color);
    if let Some(bg) = style.bg {
        border = border.bg(bg);
    }
    border
}

fn day_framed_lines(period: PeriodKey, cost: Option<f64>) -> Vec<Line<'static>> {
    let day = local_date(period.start_millis)
        .map(|date| date.day().to_string())
        .unwrap_or_else(|| "?".to_string());
    vec![Line::from(day), Line::from(cost_label(cost))]
}

fn period_framed_lines(period: PeriodKey, cost: Option<f64>) -> Vec<Line<'static>> {
    vec![
        Line::from(compact_period_label(period)),
        Line::from(cost_label(cost)),
    ]
}

fn period_cell(
    period: PeriodKey,
    label: String,
    cost: Option<f64>,
    max_cost: f64,
    selected: bool,
    in_primary_range: bool,
) -> Cell<'static> {
    Cell::from(label).style(period_style(
        period,
        cost,
        max_cost,
        selected,
        in_primary_range,
    ))
}

fn period_style(
    period: PeriodKey,
    cost: Option<f64>,
    max_cost: f64,
    selected: bool,
    in_primary_range: bool,
) -> Style {
    if selected {
        return Style::default()
            .fg(Color::Black)
            .bg(CALENDAR_ACCENT)
            .add_modifier(Modifier::BOLD);
    }

    let base = if in_primary_range {
        Style::default().fg(match period.scale {
            CalendarScale::Day => TEXT,
            CalendarScale::Week => TOKENS,
            CalendarScale::Month => COST,
        })
    } else {
        Style::default().fg(MUTED)
    };

    let Some(cost) = cost else {
        return base;
    };
    let Some(bucket) = heat_bucket(cost, max_cost) else {
        return base;
    };

    if in_primary_range {
        let fg = if bucket >= 5 {
            Color::Black
        } else {
            Color::Indexed(230)
        };
        base.fg(fg).bg(Color::Indexed(HEAT_PRIMARY_BG[bucket]))
    } else {
        base.bg(Color::Indexed(HEAT_DIM_BG[bucket]))
    }
}

fn calendar_max_cost(app: &AppState) -> f64 {
    app.calendar
        .visible_periods
        .iter()
        .filter_map(|period| app.calendar_cost(*period))
        .fold(0.0, f64::max)
}

fn heat_bucket(cost: f64, max_cost: f64) -> Option<usize> {
    if cost <= 0.0 || max_cost <= 0.0 {
        return None;
    }

    let ratio = (cost / max_cost).clamp(0.0, 1.0).sqrt();
    Some(
        ((ratio * (HEAT_PRIMARY_BG.len() - 1) as f64).round() as usize)
            .clamp(0, HEAT_PRIMARY_BG.len() - 1),
    )
}

fn day_cell_label(period: PeriodKey, cost: Option<f64>) -> String {
    let day = local_date(period.start_millis)
        .map(|date| date.day().to_string())
        .unwrap_or_else(|| "?".to_string());
    format!("{day:>2} {}", cost_label(cost))
}

fn period_cell_label(period: PeriodKey, cost: Option<f64>) -> String {
    format!("{} {}", compact_period_label(period), cost_label(cost))
}

fn cost_label(cost: Option<f64>) -> String {
    cost.map(format::cost).unwrap_or_else(|| "--".to_string())
}

fn overview_title(scale: CalendarScale, selected: PeriodKey) -> String {
    match scale {
        CalendarScale::Day => local_date(selected.start_millis)
            .map(|date| date.format("%B %Y").to_string())
            .unwrap_or_else(|| "selected month".to_string()),
        CalendarScale::Week => "rolling weeks".to_string(),
        CalendarScale::Month => local_date(selected.start_millis)
            .map(|date| date.year().to_string())
            .unwrap_or_else(|| "selected year".to_string()),
    }
}

fn compact_period_label(period: PeriodKey) -> String {
    let Some(start) = local_date(period.start_millis) else {
        return "period".to_string();
    };

    match period.scale {
        CalendarScale::Day => format!("{} {}", start.format("%b"), start.day()),
        CalendarScale::Week => format!("W{} {}", start.iso_week().week(), start.format("%b %d")),
        CalendarScale::Month => start.format("%b").to_string(),
    }
}

fn detail_period_label(period: PeriodKey) -> String {
    let Some(start) = local_date(period.start_millis) else {
        return period.scale.title().to_string();
    };
    let end = local_date(period.end_millis);

    match period.scale {
        CalendarScale::Day => format!("{} {}", start.format("%b"), start.day()),
        CalendarScale::Week => {
            let end_label = end
                .map(|date| format!("{} {}", date.format("%b"), date.day()))
                .unwrap_or_else(|| "next week".to_string());
            format!(
                "Week {}: {} {} - {end_label}",
                start.iso_week().week(),
                start.format("%b"),
                start.day()
            )
        }
        CalendarScale::Month => start.format("%B %Y").to_string(),
    }
}

fn local_date(millis: i64) -> Option<DateTime<Local>> {
    DateTime::from_timestamp_millis(millis).map(|value| value.with_timezone(&Local))
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

fn draw_models(
    frame: &mut Frame<'_>,
    area: Rect,
    stats: Option<&UsageStats>,
    loading: bool,
    title: &str,
) {
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
                    .title(format!(" {title} "))
                    .title_style(Style::default().fg(TITLE))
                    .border_style(Style::default().fg(BORDER)),
            )
            .style(Style::default().fg(if loading { TOKENS } else { MUTED }))
            .alignment(Alignment::Center);
        frame.render_widget(paragraph, area);
        return;
    };

    if area.width >= 112 {
        draw_wide_models(frame, area, stats, title);
    } else {
        draw_compact_models(frame, area, stats, title);
    }
}

fn draw_wide_models(frame: &mut Frame<'_>, area: Rect, stats: &UsageStats, title: &str) {
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
            .title(format!(" {title} "))
            .title_style(Style::default().fg(TITLE))
            .border_style(Style::default().fg(BORDER)),
    )
    .column_spacing(1);

    frame.render_widget(table, area);
}

fn draw_compact_models(frame: &mut Frame<'_>, area: Rect, stats: &UsageStats, title: &str) {
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
            .title(format!(" {title} "))
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
    let (mut spans, status, status_color) = match app.view {
        View::Dashboard => (
            vec![
                key_span(" Tab "),
                Span::styled(" mode ", Style::default().fg(MUTED)),
                key_span(" S-Tab "),
                Span::styled(" back ", Style::default().fg(MUTED)),
                key_span(" c "),
                Span::styled(" calendar ", Style::default().fg(MUTED)),
                key_span(" r "),
                Span::styled(" refresh ", Style::default().fg(MUTED)),
                key_span(" q "),
                Span::styled(" quit | ", Style::default().fg(MUTED)),
            ],
            dashboard_status(app),
            dashboard_status_color(app),
        ),
        View::CalendarOverview => (
            vec![
                key_span(" Tab "),
                Span::styled(" scale ", Style::default().fg(MUTED)),
                key_span(" hjkl "),
                Span::styled(" move ", Style::default().fg(MUTED)),
                key_span(" Enter "),
                Span::styled(" open ", Style::default().fg(MUTED)),
                key_span(" Esc "),
                Span::styled(" back ", Style::default().fg(MUTED)),
                key_span(" q "),
                Span::styled(" quit | ", Style::default().fg(MUTED)),
            ],
            calendar_status(app),
            calendar_status_color(app),
        ),
        View::CalendarDetail => (
            vec![
                key_span(" h/k "),
                Span::styled(" prev ", Style::default().fg(MUTED)),
                key_span(" j/l "),
                Span::styled(" next ", Style::default().fg(MUTED)),
                key_span(" Tab "),
                Span::styled(" scale ", Style::default().fg(MUTED)),
                key_span(" Esc "),
                Span::styled(" back ", Style::default().fg(MUTED)),
                key_span(" q "),
                Span::styled(" quit | ", Style::default().fg(MUTED)),
            ],
            history_status(app),
            history_status_color(app),
        ),
    };
    spans.push(Span::styled(status, Style::default().fg(status_color)));

    let text = Line::from(spans);

    frame.render_widget(Paragraph::new(text).style(Style::default().fg(MUTED)), area);
}

fn dashboard_status(app: &AppState) -> String {
    if let Some(error) = &app.error {
        format!("error: {error}")
    } else if let Some(stats) = app.current_stats() {
        let cutoff = cutoff_label(stats);
        format!("{} | {cutoff}", format::timestamp(stats.refreshed_at))
    } else if app.is_current_loading() {
        "loading".to_string()
    } else {
        "idle".to_string()
    }
}

fn dashboard_status_color(app: &AppState) -> Color {
    if app.error.is_some() {
        ERROR
    } else if app.is_current_loading() {
        TOKENS
    } else {
        MUTED
    }
}

fn calendar_status(app: &AppState) -> String {
    if let Some(error) = &app.error {
        format!("error: {error}")
    } else if app.calendar_loading {
        "loading calendar".to_string()
    } else {
        format!("selected {}", detail_period_label(app.calendar.selected))
    }
}

fn calendar_status_color(app: &AppState) -> Color {
    if app.error.is_some() {
        ERROR
    } else if app.calendar_loading {
        TOKENS
    } else {
        MUTED
    }
}

fn history_status(app: &AppState) -> String {
    if let Some(error) = &app.error {
        format!("error: {error}")
    } else if let Some(stats) = app.selected_history_stats() {
        let cutoff = cutoff_label(stats);
        format!("{} | {cutoff}", format::timestamp(stats.refreshed_at))
    } else if app.is_selected_history_loading() {
        "loading".to_string()
    } else {
        "idle".to_string()
    }
}

fn history_status_color(app: &AppState) -> Color {
    if app.error.is_some() {
        ERROR
    } else if app.is_selected_history_loading() {
        TOKENS
    } else {
        MUTED
    }
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

    if let Some(end_millis) = stats.end_millis {
        let start = local_date(cutoff_millis);
        let end = local_date(end_millis);
        return match (start, end) {
            (Some(start), Some(end)) => format!(
                "{} - {}",
                start.format("%b %d %H:%M"),
                end.format("%b %d %H:%M")
            ),
            _ => "selected range".to_string(),
        };
    }

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
        app::CalendarState,
        config::{Config, Scope},
        time_window::{self, DailyStart},
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
    fn maps_calendar_costs_to_indexed_heat_colors() {
        let period = PeriodKey {
            scale: CalendarScale::Day,
            start_millis: 1000,
            end_millis: 2000,
        };

        assert_eq!(heat_bucket(0.0, 10.0), None);
        assert_eq!(heat_bucket(10.0, 10.0), Some(HEAT_PRIMARY_BG.len() - 1));

        let style = period_style(period, Some(2.5), 10.0, false, true);
        assert!(matches!(style.bg, Some(Color::Indexed(_))));

        let dimmed = period_style(period, Some(2.5), 10.0, false, false);
        assert!(matches!(dimmed.bg, Some(Color::Indexed(_))));
        assert_eq!(dimmed.fg, Some(MUTED));
    }

    #[test]
    fn maps_tab_click_positions_to_modes() {
        let area = Rect::new(0, 0, 100, 24);

        assert_eq!(
            tab_at_position(2, 1, area),
            Some(TabTarget::Mode(Mode::Daily))
        );
        assert_eq!(
            tab_at_position(10, 1, area),
            Some(TabTarget::Mode(Mode::Weekly))
        );
        assert_eq!(
            tab_at_position(19, 1, area),
            Some(TabTarget::Mode(Mode::Monthly))
        );
        assert_eq!(
            tab_at_position(30, 1, area),
            Some(TabTarget::Mode(Mode::AllTime))
        );
        assert_eq!(tab_at_position(92, 1, area), Some(TabTarget::Calendar));
    }

    #[test]
    fn ignores_clicks_outside_tab_labels() {
        let area = Rect::new(0, 0, 100, 24);

        assert_eq!(tab_at_position(2, 0, area), None);
        assert_eq!(tab_at_position(8, 1, area), None);
        assert_eq!(tab_at_position(80, 1, area), None);
        assert_eq!(tab_at_position(2, 2, area), None);
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

    #[test]
    fn renders_calendar_overview_with_costs() {
        let selected = time_window::current_period(
            CalendarScale::Day,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            DailyStart::default(),
        )
        .unwrap();
        let mut app = app_with_calendar(selected);
        app.calendar_costs.insert(selected, 4.25);

        let output = render(&app, 120, 24);

        assert!(output.contains("Calendar"));
        assert!(output.contains("Calendar: Day | June 2026"));
        assert!(output.contains("Mon"));
        assert!(output.contains("15 $4.25"));
        assert!(output.contains("selected Jun 15"));
    }

    #[test]
    fn renders_framed_calendar_cells_when_large_enough() {
        let selected = time_window::current_period(
            CalendarScale::Day,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            DailyStart::default(),
        )
        .unwrap();
        let mut app = app_with_calendar(selected);
        app.calendar_costs.insert(selected, 4.25);

        let output = render(&app, 120, 36);

        assert!(output.contains("$4.25"));
        assert!(output.matches('┌').count() > 10);
    }

    #[test]
    fn renders_week_calendar_with_week_numbers() {
        let selected = time_window::current_period(
            CalendarScale::Week,
            Local.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap(),
            DailyStart::default(),
        )
        .unwrap();
        let mut app = app_with_calendar(selected);
        app.calendar_costs.insert(selected, 6.5);

        let output = render(&app, 120, 24);

        assert!(output.contains("W25"));
        assert!(output.contains("$6.50"));
        assert!(output.contains("selected Week 25"));
    }

    #[test]
    fn renders_calendar_detail_with_bounded_range() {
        let selected = time_window::current_period(
            CalendarScale::Day,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            DailyStart::default(),
        )
        .unwrap();
        let mut stats = sample_stats(Mode::Daily, Some(selected.start_millis));
        stats.end_millis = Some(selected.end_millis);
        let mut app = app_with_calendar(selected);
        app.view = View::CalendarDetail;
        app.history_stats.insert(selected, stats);

        let output = render(&app, 120, 24);

        assert!(output.contains("Jun 15 by model"));
        assert!(output.contains("$3.75"));
        assert!(output.contains("Jun 15 04:00 - Jun 16 04:00"));
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
            view: View::Dashboard,
            mode,
            stats: stats_by_mode,
            loading: HashSet::new(),
            calendar: test_calendar(),
            calendar_costs: HashMap::new(),
            calendar_loading: false,
            history_stats: HashMap::new(),
            history_loading: HashSet::new(),
            error: None,
            last_refresh_started: None,
            next_refresh_due: Instant::now() + Duration::from_secs(60),
        }
    }

    fn app_loading(mode: Mode) -> AppState {
        AppState {
            config: test_config(),
            view: View::Dashboard,
            mode,
            stats: HashMap::new(),
            loading: HashSet::from([mode]),
            calendar: test_calendar(),
            calendar_costs: HashMap::new(),
            calendar_loading: false,
            history_stats: HashMap::new(),
            history_loading: HashSet::new(),
            error: None,
            last_refresh_started: None,
            next_refresh_due: Instant::now() + Duration::from_secs(60),
        }
    }

    fn app_with_calendar(selected: PeriodKey) -> AppState {
        AppState {
            config: test_config(),
            view: View::CalendarOverview,
            mode: Mode::Daily,
            stats: HashMap::new(),
            loading: HashSet::new(),
            calendar: CalendarState {
                scale: selected.scale,
                selected,
                visible_periods: time_window::visible_periods(selected, DailyStart::default())
                    .unwrap(),
            },
            calendar_costs: HashMap::new(),
            calendar_loading: false,
            history_stats: HashMap::new(),
            history_loading: HashSet::new(),
            error: None,
            last_refresh_started: None,
            next_refresh_due: Instant::now() + Duration::from_secs(60),
        }
    }

    fn test_calendar() -> CalendarState {
        let selected = time_window::current_period(
            CalendarScale::Day,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            DailyStart::default(),
        )
        .unwrap();

        CalendarState {
            scale: CalendarScale::Day,
            selected,
            visible_periods: time_window::visible_periods(selected, DailyStart::default()).unwrap(),
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
            end_millis: None,
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
