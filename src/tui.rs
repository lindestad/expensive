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
