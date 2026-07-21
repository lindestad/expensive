use chrono::{DateTime, Datelike, Local};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};

use crate::{
    app::{AppState, ConfigEditorItem, GraphMetric, SourceSyncStage, SourceSyncStatus, View},
    config::{ColorTheme, ThemeScope},
    db::{ModelUsage, UsageStats, UsageTotals},
    format,
    sources::SyncMode,
    time_window::{self, CalendarScale, Mode, PeriodKey, WeekStart},
};

mod token_graph;

use token_graph::{draw_token_graph, draw_token_graph_loading, token_bucket_index_at_position};

#[derive(Clone, Copy)]
struct Palette {
    border: Color,
    text: Color,
    muted: Color,
    title: Color,
    accent: Color,
    calendar_accent: Color,
    cost: Color,
    tokens: Color,
    io: Color,
    cache: Color,
    error: Color,
    key_bg: Color,
    heat_primary_bg: [u8; 7],
    heat_dim_bg: [u8; 7],
}

#[derive(Clone, Copy)]
struct MetricStyle {
    value: Color,
    label: Color,
}

fn active_palette(app: &AppState) -> Palette {
    let themed = palette_for(app.config.color_theme);
    match app.config.theme_scope {
        ThemeScope::All => themed,
        ThemeScope::Calendar => Palette {
            heat_primary_bg: themed.heat_primary_bg,
            heat_dim_bg: themed.heat_dim_bg,
            ..palette_for(ColorTheme::Aurora)
        },
    }
}

fn palette_for(theme: ColorTheme) -> Palette {
    match theme {
        ColorTheme::Aurora => Palette {
            border: Color::Rgb(64, 74, 92),
            text: Color::Rgb(220, 226, 235),
            muted: Color::Rgb(119, 133, 154),
            title: Color::Rgb(170, 220, 255),
            accent: Color::Rgb(80, 210, 190),
            calendar_accent: Color::Rgb(218, 170, 255),
            cost: Color::Rgb(255, 196, 118),
            tokens: Color::Rgb(155, 210, 255),
            io: Color::Rgb(187, 162, 255),
            cache: Color::Rgb(146, 226, 160),
            error: Color::Rgb(255, 117, 117),
            key_bg: Color::Rgb(156, 168, 185),
            heat_primary_bg: [23, 30, 66, 101, 136, 172, 208],
            heat_dim_bg: [235, 236, 237, 238, 239, 240, 241],
        },
        ColorTheme::Ember => Palette {
            border: Color::Rgb(92, 70, 54),
            text: Color::Rgb(238, 226, 211),
            muted: Color::Rgb(160, 128, 108),
            title: Color::Rgb(255, 184, 124),
            accent: Color::Rgb(255, 143, 89),
            calendar_accent: Color::Rgb(255, 205, 112),
            cost: Color::Rgb(255, 210, 135),
            tokens: Color::Rgb(255, 162, 117),
            io: Color::Rgb(255, 128, 142),
            cache: Color::Rgb(184, 221, 139),
            error: Color::Rgb(255, 96, 96),
            key_bg: Color::Rgb(178, 130, 94),
            heat_primary_bg: [52, 88, 124, 160, 166, 202, 208],
            heat_dim_bg: [235, 236, 237, 238, 239, 240, 241],
        },
        ColorTheme::Ocean => Palette {
            border: Color::Rgb(48, 78, 96),
            text: Color::Rgb(214, 234, 238),
            muted: Color::Rgb(111, 147, 160),
            title: Color::Rgb(139, 218, 233),
            accent: Color::Rgb(80, 190, 220),
            calendar_accent: Color::Rgb(144, 199, 255),
            cost: Color::Rgb(165, 224, 220),
            tokens: Color::Rgb(132, 211, 255),
            io: Color::Rgb(166, 178, 255),
            cache: Color::Rgb(128, 226, 190),
            error: Color::Rgb(255, 107, 119),
            key_bg: Color::Rgb(108, 157, 178),
            heat_primary_bg: [17, 24, 31, 38, 44, 80, 116],
            heat_dim_bg: [235, 236, 237, 238, 239, 240, 241],
        },
        ColorTheme::Forest => Palette {
            border: Color::Rgb(58, 88, 67),
            text: Color::Rgb(220, 235, 218),
            muted: Color::Rgb(126, 154, 127),
            title: Color::Rgb(177, 228, 156),
            accent: Color::Rgb(108, 214, 139),
            calendar_accent: Color::Rgb(223, 205, 120),
            cost: Color::Rgb(231, 210, 126),
            tokens: Color::Rgb(145, 220, 172),
            io: Color::Rgb(156, 210, 215),
            cache: Color::Rgb(112, 225, 136),
            error: Color::Rgb(255, 116, 105),
            key_bg: Color::Rgb(128, 164, 120),
            heat_primary_bg: [22, 28, 34, 70, 76, 112, 148],
            heat_dim_bg: [235, 236, 237, 238, 239, 240, 241],
        },
        ColorTheme::Graphite => Palette {
            border: Color::Rgb(76, 80, 88),
            text: Color::Rgb(224, 225, 228),
            muted: Color::Rgb(132, 137, 146),
            title: Color::Rgb(210, 215, 224),
            accent: Color::Rgb(179, 186, 197),
            calendar_accent: Color::Rgb(232, 214, 160),
            cost: Color::Rgb(230, 216, 173),
            tokens: Color::Rgb(192, 204, 220),
            io: Color::Rgb(202, 194, 220),
            cache: Color::Rgb(190, 214, 190),
            error: Color::Rgb(255, 118, 118),
            key_bg: Color::Rgb(148, 153, 162),
            heat_primary_bg: [236, 237, 238, 239, 240, 245, 250],
            heat_dim_bg: [232, 233, 234, 235, 236, 237, 238],
        },
    }
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
struct HelpBinding {
    key: &'static str,
    description: &'static str,
}

#[derive(Clone, Copy)]
struct HelpColumns {
    key_width: usize,
    description_width: usize,
    left_pad: usize,
}

struct HelpContent<'a> {
    config_path: &'a str,
    control_bindings: &'a [HelpBinding],
    config_bindings: &'a [HelpBinding],
    columns: HelpColumns,
}

struct HelpDocument {
    lines: Vec<Line<'static>>,
    config_start: usize,
    config_end: usize,
    config_item_starts: Vec<usize>,
}

#[derive(Debug)]
pub struct HelpLayoutState {
    pub visible_height: usize,
    pub max_scroll: usize,
    pub config_start: usize,
    pub config_end: usize,
    pub config_item_starts: Vec<usize>,
}

#[derive(Clone, Copy)]
struct ConfigColumns {
    label_width: usize,
    left_pad: usize,
}

struct ConfigEditorLines {
    lines: Vec<Line<'static>>,
    item_starts: Vec<usize>,
}

#[derive(Clone, Copy)]
struct StatsLayout {
    summary: Rect,
    comparison: Option<Rect>,
    models: Rect,
    graph: Option<Rect>,
}

struct StatsViewState<'a> {
    stats: Option<&'a UsageStats>,
    loading: bool,
    graph_loading: bool,
    model_title: String,
    model_scroll: usize,
    selected_token_bucket: Option<usize>,
    graph_metric: GraphMetric,
    show_comparison: bool,
    first_sync: bool,
    source_sync_status: Option<&'a SourceSyncStatus>,
}

struct ModelViewState<'a> {
    stats: Option<&'a UsageStats>,
    loading: bool,
    title: &'a str,
    scroll: usize,
    first_sync: bool,
    source_sync_status: Option<&'a SourceSyncStatus>,
}

#[derive(Clone)]
struct ValueToken {
    text: String,
    style: Style,
}

#[derive(Clone, Copy)]
enum TabStyle {
    Normal,
    Dashboard,
    Calendar,
}

pub fn draw(frame: &mut Frame<'_>, app: &AppState) {
    let area = frame.area();
    let palette = active_palette(app);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(11),
            Constraint::Length(1),
        ])
        .split(area);

    draw_tabs(frame, chunks[0], app, palette);
    match app.view {
        View::Dashboard => draw_stats_view(
            frame,
            chunks[1],
            StatsViewState {
                stats: app.current_stats(),
                loading: app.is_current_loading(),
                graph_loading: app.is_current_graph_loading(),
                model_title: format!("{} by model", app.mode.title()),
                model_scroll: app.dashboard_model_scroll,
                selected_token_bucket: app.dashboard_token_bucket,
                graph_metric: app.graph_metric,
                show_comparison: app.config.show_comparison,
                first_sync: app.first_sync && app.source_sync.is_some(),
                source_sync_status: app.source_sync_status.as_ref(),
            },
            palette,
        ),
        View::CalendarOverview => draw_calendar_overview(frame, chunks[1], app, palette),
        View::CalendarDetail => draw_stats_view(
            frame,
            chunks[1],
            StatsViewState {
                stats: app.selected_history_stats(),
                loading: app.is_selected_history_loading(),
                graph_loading: app.is_selected_history_graph_loading(),
                model_title: format!(
                    "{} by model",
                    detail_period_label(app.calendar.selected, app.config.week_start)
                ),
                model_scroll: app.history_model_scroll,
                selected_token_bucket: app.history_token_bucket,
                graph_metric: app.graph_metric,
                show_comparison: app.config.show_comparison,
                first_sync: false,
                source_sync_status: None,
            },
            palette,
        ),
    }
    draw_footer(frame, chunks[2], app, palette);

    if app.show_help {
        draw_help(frame, area, app, palette);
    }
    if app.scope_picker.is_some() {
        draw_scope_picker(frame, area, app, palette);
    }
    if app.provider_picker.is_some() {
        draw_provider_picker(frame, area, app, palette);
    }
    if app.alias_editor.is_some() {
        draw_alias_editor(frame, area, app, palette);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TabTarget {
    Mode(Mode),
    Calendar,
}

pub fn calendar_period_at_position(
    column: u16,
    row: u16,
    area: Rect,
    app: &AppState,
) -> Option<PeriodKey> {
    if app.view != View::CalendarOverview {
        return None;
    }

    let body = main_body_area(area);
    let inner = body.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    if !rect_contains(inner, column, row) {
        return None;
    }

    match app.calendar.scale {
        CalendarScale::Day => day_period_at_position(column, row, inner, app),
        CalendarScale::Week => period_grid_at_position(column, row, inner, app, 4),
        CalendarScale::Month => period_grid_at_position(column, row, inner, app, 3),
    }
}

pub fn model_breakdown_area(area: Rect, app: &AppState) -> Option<Rect> {
    stats_for_view(app).map(|stats| {
        stats_view_layout(
            main_body_area(area),
            Some(stats),
            graph_available_for_view(app),
            app.config.show_comparison,
        )
        .models
    })
}

pub fn model_breakdown_at_position(column: u16, row: u16, area: Rect, app: &AppState) -> bool {
    model_breakdown_area(area, app)
        .map(|area| rect_contains(area, column, row))
        .unwrap_or(false)
}

pub fn model_breakdown_max_scroll(area: Rect, row_count: usize) -> usize {
    row_count.saturating_sub(model_breakdown_visible_rows(area))
}

pub fn token_graph_area(area: Rect, app: &AppState) -> Option<Rect> {
    stats_for_view(app).and_then(|stats| {
        stats_view_layout(
            main_body_area(area),
            Some(stats),
            graph_available_for_view(app),
            app.config.show_comparison,
        )
        .graph
    })
}

pub fn token_graph_capacity_area(area: Rect, app: &AppState) -> Option<Rect> {
    stats_for_view(app).and_then(|stats| {
        if stats.totals.messages == 0 {
            return None;
        }
        stats_view_layout(
            main_body_area(area),
            Some(stats),
            true,
            app.config.show_comparison,
        )
        .graph
    })
}

pub fn token_bucket_at_position(
    column: u16,
    row: u16,
    area: Rect,
    app: &AppState,
) -> Option<usize> {
    let stats = stats_for_view(app)?;
    let graph_area = token_graph_area(area, app)?;
    token_bucket_index_at_position(column, row, graph_area, stats)
}

fn stats_for_view(app: &AppState) -> Option<&UsageStats> {
    match app.view {
        View::Dashboard => app.current_stats(),
        View::CalendarDetail => app.selected_history_stats(),
        View::CalendarOverview => None,
    }
}

fn graph_available_for_view(app: &AppState) -> bool {
    stats_for_view(app)
        .map(|stats| !stats.token_buckets.is_empty())
        .unwrap_or(false)
        || match app.view {
            View::Dashboard => app.is_current_graph_loading(),
            View::CalendarDetail => app.is_selected_history_graph_loading(),
            View::CalendarOverview => false,
        }
}

fn draw_help(frame: &mut Frame<'_>, area: Rect, app: &AppState, palette: Palette) {
    let modal = centered_rect(area, 96, 30);
    frame.render_widget(Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ")
        .title_alignment(Alignment::Center)
        .title_style(
            Style::default()
                .fg(palette.title)
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(palette.border));
    let inner = help_inner_area(area);
    frame.render_widget(block, modal);

    let layout = help_layout_state(area, app);
    let scroll = app.help_scroll.min(layout.max_scroll);
    let document = help_document(
        app,
        palette,
        inner.width as usize,
        help_config_visible(scroll, &layout),
    );
    let lines = document
        .lines
        .into_iter()
        .skip(scroll)
        .take(layout.visible_height)
        .collect::<Vec<_>>();

    let paragraph = Paragraph::new(lines).style(Style::default().fg(palette.text));

    frame.render_widget(paragraph, inner);
}

fn draw_scope_picker(frame: &mut Frame<'_>, area: Rect, app: &AppState, palette: Palette) {
    let Some(picker) = &app.scope_picker else {
        return;
    };
    let option_count = app.projects.len().saturating_add(2);
    let modal = centered_rect(area, 88, (option_count.saturating_add(4) as u16).min(24));
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Project scope ")
        .title_alignment(Alignment::Center)
        .title_style(
            Style::default()
                .fg(palette.title)
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(palette.border));
    let inner = modal.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    frame.render_widget(block, modal);

    let visible_options = inner.height.saturating_sub(2) as usize;
    let scroll = picker
        .selection
        .saturating_add(1)
        .saturating_sub(visible_options)
        .min(option_count.saturating_sub(visible_options));
    let mut options = vec![
        (
            "All projects".to_string(),
            "aggregate every indexed project".to_string(),
        ),
        (
            "Current directory".to_string(),
            app.config.current_directory.display().to_string(),
        ),
    ];
    options.extend(
        app.projects
            .iter()
            .map(|project| (project.name.clone(), project.worktree.clone())),
    );
    let mut lines = options
        .into_iter()
        .enumerate()
        .skip(scroll)
        .take(visible_options)
        .map(|(index, (label, detail))| {
            let selected = index == picker.selection;
            let marker = if selected { ">" } else { " " };
            let style = if selected {
                Style::default()
                    .fg(palette.calendar_accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.text)
            };
            two_column_picker_line(
                marker,
                &label,
                &detail,
                inner.width as usize,
                style,
                Style::default().fg(palette.muted),
                "  ",
            )
        })
        .collect::<Vec<_>>();
    lines.push(Line::from(""));
    lines.push(
        Line::from(vec![
            key_span(" Enter ", palette),
            Span::styled(" select  ", Style::default().fg(palette.muted)),
            key_span(" Esc ", palette),
            Span::styled(" cancel", Style::default().fg(palette.muted)),
        ])
        .alignment(Alignment::Center),
    );
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_alias_editor(frame: &mut Frame<'_>, area: Rect, app: &AppState, palette: Palette) {
    let Some(editor) = &app.alias_editor else {
        return;
    };
    let modal = centered_rect(area, 92, 24);
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", editor.kind.title()))
        .title_alignment(Alignment::Center)
        .title_style(
            Style::default()
                .fg(palette.title)
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(palette.border));
    let inner = modal.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    frame.render_widget(block, modal);

    if let Some(input) = &editor.input {
        let source_style =
            alias_input_style(input.field == crate::app::AliasInputField::Source, palette);
        let alias_style =
            alias_input_style(input.field == crate::app::AliasInputField::Alias, palette);
        let lines = vec![
            Line::from(Span::styled(
                "Raw database values are preserved; aliases only change labels.",
                Style::default().fg(palette.muted),
            )),
            Line::from(""),
            Line::from(Span::styled(
                editor.kind.source_label(),
                Style::default().fg(palette.muted),
            )),
            Line::from(Span::styled(format!(" {} ", input.source), source_style)),
            Line::from(""),
            Line::from(Span::styled(
                "Display alias",
                Style::default().fg(palette.muted),
            )),
            Line::from(Span::styled(format!(" {} ", input.alias), alias_style)),
            Line::from(""),
            Line::from(vec![
                key_span(" Tab ", palette),
                Span::styled(" field  ", Style::default().fg(palette.muted)),
                key_span(" Enter ", palette),
                Span::styled(" next/save  ", Style::default().fg(palette.muted)),
                key_span(" Esc ", palette),
                Span::styled(" cancel", Style::default().fg(palette.muted)),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let mut entries = app.alias_entries(editor.kind);
    let entry_count = entries.len();
    entries.push(("+ add alias".to_string(), String::new()));
    let visible_entries = inner.height.saturating_sub(3) as usize;
    let scroll = editor
        .selection
        .saturating_add(1)
        .saturating_sub(visible_entries)
        .min(entries.len().saturating_sub(visible_entries));
    let mut lines = entries
        .into_iter()
        .enumerate()
        .skip(scroll)
        .take(visible_entries)
        .map(|(index, (source, alias))| {
            let selected = index == editor.selection;
            let style = if selected {
                Style::default()
                    .fg(palette.calendar_accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.text)
            };
            let marker = if selected { ">" } else { " " };
            if index == entry_count {
                Line::from(Span::styled(format!("{marker} {source}"), style))
            } else {
                two_column_picker_line(
                    marker,
                    &source,
                    &alias,
                    inner.width as usize,
                    style,
                    Style::default().fg(palette.accent),
                    "  →  ",
                )
            }
        })
        .collect::<Vec<_>>();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        key_span(" Enter ", palette),
        Span::styled(" edit/add  ", Style::default().fg(palette.muted)),
        key_span(" d ", palette),
        Span::styled(" delete  ", Style::default().fg(palette.muted)),
        key_span(" Esc ", palette),
        Span::styled(" close", Style::default().fg(palette.muted)),
    ]));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_provider_picker(frame: &mut Frame<'_>, area: Rect, app: &AppState, palette: Palette) {
    let Some(picker) = &app.provider_picker else {
        return;
    };
    let modal = centered_rect(area, 76, 22);
    frame.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Provider visibility ")
        .title_alignment(Alignment::Center)
        .title_style(
            Style::default()
                .fg(palette.title)
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(palette.border));
    let inner = modal.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    frame.render_widget(block, modal);

    let entries = app.provider_entries();
    let list_height = inner.height.saturating_sub(4) as usize;
    let scroll = picker
        .selection
        .saturating_add(1)
        .saturating_sub(list_height)
        .min(entries.len().saturating_sub(list_height));
    let mut lines = vec![Line::from(Span::styled(
        "Checked providers are included in every view and report.",
        Style::default().fg(palette.muted),
    ))];
    if entries.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "No providers have been indexed yet.",
            Style::default().fg(palette.muted),
        )));
    } else {
        lines.extend(
            entries
                .iter()
                .enumerate()
                .skip(scroll)
                .take(list_height)
                .map(|(index, provider)| {
                    let selected = index == picker.selection;
                    let visible = !app.config.hidden_providers.contains(provider);
                    let style = if selected {
                        Style::default()
                            .fg(palette.calendar_accent)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(palette.text)
                    };
                    Line::from(Span::styled(
                        fit_text_ellipsis(
                            &format!(
                                "{} [{}] {provider}",
                                if selected { ">" } else { " " },
                                if visible { "x" } else { " " }
                            ),
                            inner.width as usize,
                        ),
                        style,
                    ))
                }),
        );
    }
    while lines.len() < inner.height.saturating_sub(1) as usize {
        lines.push(Line::from(""));
    }
    lines.push(
        Line::from(vec![
            key_span(" Space ", palette),
            Span::styled(" toggle  ", Style::default().fg(palette.muted)),
            key_span(" Esc ", palette),
            Span::styled(" done", Style::default().fg(palette.muted)),
        ])
        .alignment(Alignment::Center),
    );
    frame.render_widget(Paragraph::new(lines), inner);
}

fn alias_input_style(active: bool, palette: Palette) -> Style {
    if active {
        Style::default()
            .fg(Color::Black)
            .bg(palette.calendar_accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text).bg(Color::Rgb(35, 40, 48))
    }
}

fn two_column_picker_line(
    marker: &str,
    label: &str,
    detail: &str,
    width: usize,
    label_style: Style,
    detail_style: Style,
    separator: &'static str,
) -> Line<'static> {
    let prefix = format!("{marker} ");
    if detail.is_empty() {
        return Line::from(Span::styled(
            fit_text_ellipsis(&format!("{prefix}{label}"), width),
            label_style,
        ));
    }

    let full_width = text_width(&prefix)
        .saturating_add(text_width(label))
        .saturating_add(text_width(separator))
        .saturating_add(text_width(detail));
    if full_width <= width {
        return Line::from(vec![
            Span::styled(format!("{prefix}{label}"), label_style),
            Span::styled(separator, Style::default()),
            Span::styled(detail.to_string(), detail_style),
        ]);
    }

    let content_width = width
        .saturating_sub(text_width(&prefix))
        .saturating_sub(text_width(separator));
    let detail_width = text_width(detail).min(content_width / 2);
    let label_width = content_width.saturating_sub(detail_width);
    Line::from(vec![
        Span::styled(prefix, label_style),
        Span::styled(fit_text_ellipsis(label, label_width), label_style),
        Span::styled(separator, Style::default()),
        Span::styled(fit_text_ellipsis(detail, detail_width), detail_style),
    ])
}

pub fn help_layout_state(area: Rect, app: &AppState) -> HelpLayoutState {
    let inner = help_inner_area(area);
    let document = help_document(app, active_palette(app), inner.width as usize, false);
    let visible_height = inner.height as usize;
    let max_scroll = document.lines.len().saturating_sub(visible_height);

    HelpLayoutState {
        visible_height,
        max_scroll,
        config_start: document.config_start,
        config_end: document.config_end,
        config_item_starts: document.config_item_starts,
    }
}

fn help_document(
    app: &AppState,
    palette: Palette,
    width: usize,
    highlight_config: bool,
) -> HelpDocument {
    let config_path = app
        .config
        .config_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "config directory unavailable".to_string());
    let control_bindings = control_help_bindings();
    let config_bindings = config_help_bindings();
    let all_bindings = control_bindings
        .iter()
        .chain(config_bindings.iter())
        .copied()
        .collect::<Vec<_>>();
    let content = HelpContent {
        config_path: &config_path,
        control_bindings: &control_bindings,
        config_bindings: &config_bindings,
        columns: help_columns(&all_bindings, width),
    };
    let mut lines = vec![section_title("Controls", palette), Line::from("")];
    lines.extend(help_binding_lines(
        content.control_bindings,
        content.columns,
        palette,
    ));
    lines.extend([section_title("Config", palette), Line::from("")]);
    lines.extend(help_binding_lines(
        content.config_bindings,
        content.columns,
        palette,
    ));
    let config_start = lines.len();
    let config_editor = config_editor_lines(app, palette, width, highlight_config);
    let config_item_starts = config_editor
        .item_starts
        .iter()
        .map(|start| config_start + start)
        .collect::<Vec<_>>();
    lines.extend(config_editor.lines);
    let config_end = lines.len();

    if app.config_notice.is_some() {
        lines.push(config_notice_line(app, palette));
    }
    lines.extend([
        Line::from(""),
        config_line("file", content.config_path.to_string(), width, palette),
        Line::from(""),
        help_footer_line(palette),
    ]);

    HelpDocument {
        lines,
        config_start,
        config_end,
        config_item_starts,
    }
}

fn help_config_visible(scroll: usize, layout: &HelpLayoutState) -> bool {
    layout.visible_height > 0
        && layout.config_end > layout.config_start
        && scroll.saturating_add(layout.visible_height) > layout.config_start
        && scroll < layout.config_end
}

fn help_inner_area(area: Rect) -> Rect {
    centered_rect(area, 96, 30).inner(Margin {
        horizontal: 3,
        vertical: 1,
    })
}

fn control_help_bindings() -> [HelpBinding; 10] {
    [
        HelpBinding {
            key: "Tab / Shift+Tab",
            description: "switch dashboard window or calendar scale",
        },
        HelpBinding {
            key: "c",
            description: "open Calendar from the dashboard",
        },
        HelpBinding {
            key: "hjkl / arrows",
            description: "move Calendar selection",
        },
        HelpBinding {
            key: "Enter",
            description: "open selected Calendar period",
        },
        HelpBinding {
            key: "Esc",
            description: "back one level; close help",
        },
        HelpBinding {
            key: "r",
            description: "refresh current view and sources",
        },
        HelpBinding {
            key: "R",
            description: "rebuild usage index from sources",
        },
        HelpBinding {
            key: "g",
            description: "toggle token and cost graph",
        },
        HelpBinding {
            key: "p",
            description: "choose project scope",
        },
        HelpBinding {
            key: "q",
            description: "quit",
        },
    ]
}

fn config_help_bindings() -> [HelpBinding; 3] {
    [
        HelpBinding {
            key: "j/k",
            description: "select a config row",
        },
        HelpBinding {
            key: "Space / Enter",
            description: "toggle or cycle the selected value",
        },
        HelpBinding {
            key: "h/l",
            description: "cycle choices",
        },
    ]
}

fn help_footer_line(palette: Palette) -> Line<'static> {
    Line::from(vec![
        key_span(" ? ", palette),
        Span::styled(" close ", Style::default().fg(palette.muted)),
        key_span(" Esc ", palette),
        Span::styled(" close ", Style::default().fg(palette.muted)),
        key_span(" q ", palette),
        Span::styled(" quit", Style::default().fg(palette.muted)),
    ])
    .alignment(Alignment::Center)
}

fn main_body_area(area: Rect) -> Rect {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(11),
            Constraint::Length(1),
        ])
        .split(area)[1]
}

fn day_period_at_position(column: u16, row: u16, area: Rect, app: &AppState) -> Option<PeriodKey> {
    if can_draw_framed_grid(area, 6, 7, 1) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(area);
        return framed_grid_period_at_position(
            column,
            row,
            chunks[1],
            &app.calendar.visible_periods,
            7,
            6,
        );
    }

    compact_grid_period_at_position(
        column,
        row,
        Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(1),
        ),
        &app.calendar.visible_periods,
        7,
        6,
        &[
            Constraint::Percentage(14),
            Constraint::Percentage(14),
            Constraint::Percentage(14),
            Constraint::Percentage(14),
            Constraint::Percentage(14),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
        ],
    )
}

fn period_grid_at_position(
    column: u16,
    row: u16,
    area: Rect,
    app: &AppState,
    columns: usize,
) -> Option<PeriodKey> {
    let rows = app.calendar.visible_periods.len().div_ceil(columns);
    if can_draw_framed_grid(area, rows, columns, 0) {
        return framed_grid_period_at_position(
            column,
            row,
            area,
            &app.calendar.visible_periods,
            columns,
            rows,
        );
    }

    let constraints = (0..columns)
        .map(|_| Constraint::Ratio(1, columns as u32))
        .collect::<Vec<_>>();
    compact_grid_period_at_position(
        column,
        row,
        area,
        &app.calendar.visible_periods,
        columns,
        rows,
        &constraints,
    )
}

fn framed_grid_period_at_position(
    column: u16,
    row: u16,
    area: Rect,
    periods: &[PeriodKey],
    columns: usize,
    rows: usize,
) -> Option<PeriodKey> {
    if !rect_contains(area, column, row) {
        return None;
    }

    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(even_constraints(rows))
        .split(area);
    let row_idx = row_areas
        .iter()
        .position(|area| rect_contains(*area, column, row))?;
    let column_areas = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(even_constraints(columns))
        .split(row_areas[row_idx]);
    let column_idx = column_areas
        .iter()
        .position(|area| rect_contains(*area, column, row))?;

    periods.get(row_idx * columns + column_idx).copied()
}

fn compact_grid_period_at_position(
    column: u16,
    row: u16,
    area: Rect,
    periods: &[PeriodKey],
    columns: usize,
    rows: usize,
    column_constraints: &[Constraint],
) -> Option<PeriodKey> {
    if !rect_contains(area, column, row) {
        return None;
    }

    let row_idx = row.checked_sub(area.y)? as usize;
    if row_idx >= rows {
        return None;
    }

    let column_idx = column_at_position(column, area, column_constraints, 1)?;
    periods.get(row_idx * columns + column_idx).copied()
}

fn column_at_position(
    column: u16,
    area: Rect,
    constraints: &[Constraint],
    spacing: u16,
) -> Option<usize> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .spacing(spacing)
        .split(area)
        .iter()
        .position(|area| column >= area.x && column < area.x.saturating_add(area.width))
}

fn rect_contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

fn help_columns(bindings: &[HelpBinding], available_width: usize) -> HelpColumns {
    const HELP_GAP: usize = 2;
    const MIN_DESCRIPTION_WIDTH: usize = 18;

    let widest_key = bindings
        .iter()
        .map(|binding| text_width(binding.key))
        .max()
        .unwrap_or(1);
    let max_key_width = available_width
        .saturating_sub(HELP_GAP + MIN_DESCRIPTION_WIDTH)
        .max(1);
    let key_width = widest_key.min(max_key_width).max(1);
    let description_width = available_width.saturating_sub(key_width + HELP_GAP).max(1);
    let table_width = bindings
        .iter()
        .map(|binding| {
            key_width + HELP_GAP + text_width(binding.description).min(description_width)
        })
        .max()
        .unwrap_or(key_width + HELP_GAP)
        .min(available_width);

    HelpColumns {
        key_width,
        description_width,
        left_pad: available_width.saturating_sub(table_width) / 2,
    }
}

fn help_binding_lines(
    bindings: &[HelpBinding],
    columns: HelpColumns,
    palette: Palette,
) -> Vec<Line<'static>> {
    bindings
        .iter()
        .flat_map(|binding| help_binding_line(*binding, columns, palette))
        .collect()
}

fn help_binding_line(
    binding: HelpBinding,
    columns: HelpColumns,
    palette: Palette,
) -> Vec<Line<'static>> {
    wrap_text(binding.description, columns.description_width)
        .into_iter()
        .enumerate()
        .map(|(idx, description)| {
            if idx == 0 {
                Line::from(vec![
                    Span::raw(" ".repeat(columns.left_pad)),
                    Span::styled(
                        format!(
                            "{:>width$}",
                            fit_text_ellipsis(
                                compact_help_key(binding.key, columns.key_width),
                                columns.key_width,
                            ),
                            width = columns.key_width
                        ),
                        Style::default()
                            .fg(palette.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(description, Style::default().fg(palette.text)),
                ])
            } else {
                Line::from(vec![
                    Span::raw(" ".repeat(columns.left_pad + columns.key_width + 2)),
                    Span::styled(description, Style::default().fg(palette.text)),
                ])
            }
        })
        .collect()
}

fn compact_help_key(key: &'static str, available_width: usize) -> &'static str {
    if text_width(key) <= available_width {
        return key;
    }
    match key {
        "Tab / Shift+Tab" => "Tab / S-Tab",
        "hjkl / arrows" => "hjkl/arrows",
        "Space / Enter" => "Space/Enter",
        _ => key,
    }
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        let word_width = text_width(word);
        if current.is_empty() {
            if word_width <= width {
                current.push_str(word);
            } else {
                lines.extend(split_long_word(word, width));
            }
            continue;
        }

        if text_width(&current) + 1 + word_width <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current);
            current = String::new();
            if word_width <= width {
                current.push_str(word);
            } else {
                lines.extend(split_long_word(word, width));
            }
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn split_long_word(word: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for ch in word.chars() {
        current.push(ch);
        if text_width(&current) >= width {
            lines.push(current);
            current = String::new();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn fit_text_ellipsis(text: &str, width: usize) -> String {
    if text_width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".to_string();
    }

    format!("{}…", text.chars().take(width - 1).collect::<String>())
}

fn text_width(text: &str) -> usize {
    text.chars().count()
}

fn section_title(title: &'static str, palette: Palette) -> Line<'static> {
    Line::from(Span::styled(
        title,
        Style::default()
            .fg(palette.title)
            .add_modifier(Modifier::BOLD),
    ))
    .alignment(Alignment::Center)
}

fn config_line(
    label: &'static str,
    value: String,
    available_width: usize,
    palette: Palette,
) -> Line<'static> {
    const CONFIG_LABEL_WIDTH: usize = 17;
    let label_width = CONFIG_LABEL_WIDTH.min(available_width / 2);
    let prefix_width = label_width.saturating_add(2);

    Line::from(vec![
        Span::styled(
            format!("{label:>label_width$}  "),
            Style::default().fg(palette.muted),
        ),
        Span::styled(
            fit_text_ellipsis(&value, available_width.saturating_sub(prefix_width)),
            Style::default().fg(palette.text),
        ),
    ])
    .alignment(Alignment::Center)
}

fn centered_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.saturating_sub(4).min(max_width).max(40);
    let height = area.height.saturating_sub(4).min(max_height).max(12);
    let width = width.min(area.width);
    let height = height.min(area.height);

    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn config_editor_lines(
    app: &AppState,
    palette: Palette,
    available_width: usize,
    show_selection: bool,
) -> ConfigEditorLines {
    let items = ConfigEditorItem::ALL;
    let columns = config_columns(&items, app, available_width);
    let mut lines = Vec::new();
    let mut item_starts = Vec::new();

    for item in items {
        item_starts.push(lines.len());
        lines.extend(config_editor_item_lines(
            item,
            app,
            columns,
            palette,
            available_width,
            show_selection,
        ));
    }

    ConfigEditorLines { lines, item_starts }
}

fn config_columns(
    items: &[ConfigEditorItem],
    app: &AppState,
    available_width: usize,
) -> ConfigColumns {
    const CONFIG_GAP: usize = 2;
    const MARKER_WIDTH: usize = 2;

    let label_width = items
        .iter()
        .map(|item| text_width(item.label()))
        .max()
        .unwrap_or(1);
    let value_width = items
        .iter()
        .map(|item| config_value_width(*item, app))
        .max()
        .unwrap_or(1);
    let table_width = MARKER_WIDTH
        .saturating_add(label_width)
        .saturating_add(CONFIG_GAP)
        .saturating_add(value_width)
        .min(available_width);

    ConfigColumns {
        label_width,
        left_pad: available_width.saturating_sub(table_width) / 2,
    }
}

impl ConfigColumns {
    fn value_indent(self) -> usize {
        self.left_pad
            .saturating_add(self.label_width)
            .saturating_add(4)
    }
}

fn config_value_width(item: ConfigEditorItem, app: &AppState) -> usize {
    match item {
        ConfigEditorItem::AutoRefresh => {
            if app.config.auto_refresh {
                text_width(" [x]  refreshes automatically")
            } else {
                text_width(" [ ]  manual refresh only")
            }
        }
        ConfigEditorItem::DailyStart => text_width(&format!(" {} ", app.config.daily_start)),
        ConfigEditorItem::RefreshSeconds => {
            text_width(&format!(" {}s ", app.config.refresh_interval.as_secs()))
        }
        ConfigEditorItem::WeekStart => text_width(" monday  sunday "),
        ConfigEditorItem::ColorTheme => text_width(" aurora  ember  ocean  forest  graphite "),
        ConfigEditorItem::ThemeScope => text_width(" calendar  all "),
        ConfigEditorItem::ShowComparison => {
            if app.config.show_comparison {
                text_width(" [x]  visible")
            } else {
                text_width(" [ ]  hidden")
            }
        }
        ConfigEditorItem::EstimateApiCost => {
            if app.config.estimate_api_cost {
                text_width(" [x]  include API estimates")
            } else {
                text_width(" [ ]  actual costs only")
            }
        }
        ConfigEditorItem::HiddenProviders => text_width(&format!(
            " {} hidden  edit ",
            app.config.hidden_providers.len()
        )),
        ConfigEditorItem::ProviderAliases => text_width(&format!(
            " {} aliases  edit ",
            app.config.aliases.providers.len()
        )),
        ConfigEditorItem::ModelAliases => text_width(&format!(
            " {} aliases  edit ",
            app.config.aliases.models.len()
        )),
    }
}

fn config_editor_item_lines(
    item: ConfigEditorItem,
    app: &AppState,
    columns: ConfigColumns,
    palette: Palette,
    available_width: usize,
    show_selection: bool,
) -> Vec<Line<'static>> {
    let selected = show_selection && app.selected_config_item() == item;
    let tokens = config_value_tokens(item, app, palette);
    let value_indent = columns.value_indent();
    let value_width = available_width.saturating_sub(value_indent).max(1);
    let mut lines = Vec::new();
    let mut spans = vec![
        Span::raw(" ".repeat(columns.left_pad)),
        config_editor_label(item.label(), selected, columns.label_width, palette),
    ];
    let mut current_width = 0;

    for token in tokens {
        let token_width = text_width(&token.text);
        let separator_width = usize::from(current_width > 0);
        if current_width > 0 && current_width + separator_width + token_width > value_width {
            lines.push(Line::from(spans));
            spans = vec![Span::raw(" ".repeat(value_indent))];
            current_width = 0;
        }

        if current_width > 0 {
            spans.push(Span::raw(" "));
            current_width += 1;
        }

        spans.push(Span::styled(token.text, token.style));
        current_width += token_width;
    }

    lines.push(Line::from(spans));
    lines
}

fn config_editor_label(
    label: &'static str,
    selected: bool,
    label_width: usize,
    palette: Palette,
) -> Span<'static> {
    let marker = if selected { ">" } else { " " };
    let style = if selected {
        Style::default()
            .fg(palette.calendar_accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.muted)
    };

    Span::styled(format!("{marker} {label:>label_width$}  "), style)
}

fn config_value_tokens(
    item: ConfigEditorItem,
    app: &AppState,
    palette: Palette,
) -> Vec<ValueToken> {
    match item {
        ConfigEditorItem::AutoRefresh => {
            let mut tokens = vec![ValueToken {
                text: if app.config.auto_refresh {
                    " [x] ".to_string()
                } else {
                    " [ ] ".to_string()
                },
                style: checkbox_style(app.config.auto_refresh, palette),
            }];
            let label = if app.config.auto_refresh {
                ["refreshes", "automatically"]
            } else {
                ["manual", "refresh"]
            };
            tokens.extend(label.into_iter().map(|word| ValueToken {
                text: word.to_string(),
                style: Style::default().fg(palette.muted),
            }));
            if !app.config.auto_refresh {
                tokens.push(ValueToken {
                    text: "only".to_string(),
                    style: Style::default().fg(palette.muted),
                });
            }
            tokens
        }
        ConfigEditorItem::DailyStart => vec![config_value_token(
            app.config.daily_start.to_string(),
            palette,
        )],
        ConfigEditorItem::RefreshSeconds => vec![config_value_token(
            format!("{}s", app.config.refresh_interval.as_secs()),
            palette,
        )],
        ConfigEditorItem::WeekStart => option_tokens(
            &[
                (app.config.week_start == WeekStart::Monday, "monday"),
                (app.config.week_start == WeekStart::Sunday, "sunday"),
            ],
            palette,
        ),
        ConfigEditorItem::ColorTheme => option_tokens(
            &[
                (app.config.color_theme == ColorTheme::Aurora, "aurora"),
                (app.config.color_theme == ColorTheme::Ember, "ember"),
                (app.config.color_theme == ColorTheme::Ocean, "ocean"),
                (app.config.color_theme == ColorTheme::Forest, "forest"),
                (app.config.color_theme == ColorTheme::Graphite, "graphite"),
            ],
            palette,
        ),
        ConfigEditorItem::ThemeScope => option_tokens(
            &[
                (app.config.theme_scope == ThemeScope::Calendar, "calendar"),
                (app.config.theme_scope == ThemeScope::All, "all"),
            ],
            palette,
        ),
        ConfigEditorItem::ShowComparison => vec![
            ValueToken {
                text: if app.config.show_comparison {
                    " [x] ".to_string()
                } else {
                    " [ ] ".to_string()
                },
                style: checkbox_style(app.config.show_comparison, palette),
            },
            ValueToken {
                text: if app.config.show_comparison {
                    "visible".to_string()
                } else {
                    "hidden".to_string()
                },
                style: Style::default().fg(palette.muted),
            },
        ],
        ConfigEditorItem::EstimateApiCost => vec![
            ValueToken {
                text: if app.config.estimate_api_cost {
                    " [x] ".to_string()
                } else {
                    " [ ] ".to_string()
                },
                style: checkbox_style(app.config.estimate_api_cost, palette),
            },
            ValueToken {
                text: if app.config.estimate_api_cost {
                    "include API estimates".to_string()
                } else {
                    "actual costs only".to_string()
                },
                style: Style::default().fg(palette.muted),
            },
        ],
        ConfigEditorItem::HiddenProviders => vec![
            ValueToken {
                text: format!(" {} hidden ", app.config.hidden_providers.len()),
                style: Style::default().fg(palette.text),
            },
            ValueToken {
                text: " edit ".to_string(),
                style: selected_value_style(palette),
            },
        ],
        ConfigEditorItem::ProviderAliases => {
            alias_config_tokens(app.config.aliases.providers.len(), palette)
        }
        ConfigEditorItem::ModelAliases => {
            alias_config_tokens(app.config.aliases.models.len(), palette)
        }
    }
}

fn alias_config_tokens(count: usize, palette: Palette) -> Vec<ValueToken> {
    vec![
        ValueToken {
            text: format!(" {count} aliases "),
            style: Style::default().fg(palette.text),
        },
        ValueToken {
            text: " edit ".to_string(),
            style: selected_value_style(palette),
        },
    ]
}

fn config_value_token(value: String, palette: Palette) -> ValueToken {
    ValueToken {
        text: format!(" {value} "),
        style: selected_value_style(palette),
    }
}

fn option_tokens(options: &[(bool, &'static str)], palette: Palette) -> Vec<ValueToken> {
    options
        .iter()
        .map(|(active, label)| ValueToken {
            text: format!(" {label} "),
            style: if *active {
                selected_value_style(palette)
            } else {
                Style::default().fg(palette.muted)
            },
        })
        .collect()
}

fn selected_value_style(palette: Palette) -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(palette.calendar_accent)
        .add_modifier(Modifier::BOLD)
}

fn checkbox_style(checked: bool, palette: Palette) -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(if checked {
            palette.calendar_accent
        } else {
            palette.muted
        })
        .add_modifier(Modifier::BOLD)
}

fn config_notice_line(app: &AppState, palette: Palette) -> Line<'static> {
    let Some(notice) = &app.config_notice else {
        return Line::from("");
    };

    Line::from(Span::styled(
        notice.message.clone(),
        Style::default().fg(if notice.is_error {
            palette.error
        } else {
            palette.accent
        }),
    ))
    .alignment(Alignment::Center)
}

pub fn tab_at_position(column: u16, row: u16, area: Rect) -> Option<TabTarget> {
    if area.width < 3 || area.height < 3 || row != area.y.saturating_add(1) {
        return None;
    }

    let compact = compact_tabs(area.width);
    let mut x = area.x.saturating_add(1);
    for (idx, mode) in Mode::ALL.iter().enumerate() {
        let title_width = tab_width(mode_tab_label(*mode, compact));
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

    let calendar_width = tab_width(calendar_tab_label(compact));
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

fn draw_tabs(frame: &mut Frame<'_>, area: Rect, app: &AppState, palette: Palette) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" expensive ")
        .title_style(
            Style::default()
                .fg(palette.title)
                .add_modifier(Modifier::BOLD),
        )
        .border_style(Style::default().fg(palette.border));
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    frame.render_widget(block, area);

    let compact = compact_tabs(area.width);
    let calendar_label = calendar_tab_label(compact);
    let calendar_width = tab_width(calendar_label);
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
        spans.push(tab_span(
            mode_tab_label(*mode, compact),
            mode_tab_style(app, *mode),
            palette,
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), left_area);
    frame.render_widget(
        Paragraph::new(Line::from(tab_span(
            calendar_label,
            if app.view == View::Dashboard {
                TabStyle::Normal
            } else {
                TabStyle::Calendar
            },
            palette,
        )))
        .alignment(Alignment::Right),
        calendar_area,
    );
}

const CALENDAR_TAB: &str = "Calendar";

fn compact_tabs(width: u16) -> bool {
    width < 52
}

fn mode_tab_label(mode: Mode, compact: bool) -> &'static str {
    if !compact {
        return mode.title();
    }
    match mode {
        Mode::Daily => "Day",
        Mode::Weekly => "Week",
        Mode::Monthly => "Month",
        Mode::AllTime => "All",
    }
}

fn calendar_tab_label(compact: bool) -> &'static str {
    if compact {
        "Cal"
    } else {
        CALENDAR_TAB
    }
}

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

fn tab_span(label: &str, tab_style: TabStyle, palette: Palette) -> Span<'static> {
    let style = match tab_style {
        TabStyle::Normal => Style::default().fg(palette.muted),
        TabStyle::Dashboard => Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
        TabStyle::Calendar => Style::default()
            .fg(palette.calendar_accent)
            .add_modifier(Modifier::BOLD),
    };
    Span::styled(format!(" {label} "), style)
}

fn draw_stats_view(frame: &mut Frame<'_>, area: Rect, view: StatsViewState<'_>, palette: Palette) {
    let graph_available = view
        .stats
        .map(|stats| !stats.token_buckets.is_empty())
        .unwrap_or(false)
        || view.graph_loading;
    let layout = stats_view_layout(area, view.stats, graph_available, view.show_comparison);

    draw_summary(frame, layout.summary, view.stats, view.loading, palette);
    if let (Some(stats), Some(comparison)) = (view.stats, layout.comparison) {
        draw_comparison(frame, comparison, stats, palette);
    }
    draw_models(
        frame,
        layout.models,
        ModelViewState {
            stats: view.stats,
            loading: view.loading,
            title: &view.model_title,
            scroll: view.model_scroll,
            first_sync: view.first_sync,
            source_sync_status: view.source_sync_status,
        },
        palette,
    );

    if let (Some(stats), Some(graph)) = (view.stats, layout.graph) {
        if stats.token_buckets.is_empty() {
            draw_token_graph_loading(frame, graph, view.graph_metric, palette);
        } else {
            draw_token_graph(
                frame,
                graph,
                stats,
                view.selected_token_bucket,
                view.graph_metric,
                palette,
            );
        }
    }
}

fn stats_view_layout(
    area: Rect,
    stats: Option<&UsageStats>,
    graph_available: bool,
    comparison_enabled: bool,
) -> StatsLayout {
    let summary_height = summary_height(area.width);
    let show_comparison = comparison_enabled
        && stats
            .map(|stats| {
                stats.comparison.is_some() && area.height >= summary_height.saturating_add(13)
            })
            .unwrap_or(false);
    let chunks = if show_comparison {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(summary_height),
                Constraint::Length(4),
                Constraint::Min(6),
            ])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(summary_height), Constraint::Min(6)])
            .split(area)
    };
    let lower = chunks[if show_comparison { 2 } else { 1 }];
    let mut layout = StatsLayout {
        summary: chunks[0],
        comparison: show_comparison.then_some(chunks[1]),
        models: lower,
        graph: None,
    };

    let Some(stats) = stats else {
        return layout;
    };
    if !graph_available || !can_show_token_graph(lower) {
        return layout;
    }

    let model_height = model_graph_model_height(lower.height, lower.width, stats.models.len());
    let graph_height = lower.height.saturating_sub(model_height);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(model_height),
            Constraint::Length(graph_height),
        ])
        .split(lower);
    layout.models = chunks[0];
    layout.graph = Some(chunks[1]);
    layout
}

fn draw_comparison(frame: &mut Frame<'_>, area: Rect, stats: &UsageStats, palette: Palette) {
    let Some(previous) = &stats.comparison else {
        return;
    };
    let cost_delta = stats.totals.cost - previous.totals.cost;
    let token_delta = stats.totals.total_tokens() as i128 - previous.totals.total_tokens() as i128;
    let mut first = vec![
        Span::styled("cost ", Style::default().fg(palette.muted)),
        Span::styled(
            signed_cost(cost_delta),
            Style::default().fg(delta_color(cost_delta, palette)),
        ),
        Span::styled(
            format!(
                " ({})",
                percent_change(stats.totals.cost, previous.totals.cost)
            ),
            Style::default().fg(palette.muted),
        ),
        Span::styled("   tokens ", Style::default().fg(palette.muted)),
        Span::styled(
            signed_integer(token_delta),
            Style::default().fg(delta_color(token_delta as f64, palette)),
        ),
        Span::styled(
            format!(
                " ({})",
                percent_change(
                    stats.totals.total_tokens() as f64,
                    previous.totals.total_tokens() as f64,
                )
            ),
            Style::default().fg(palette.muted),
        ),
    ];
    if let Some(projected) = stats.projected_cost {
        first.extend([
            Span::styled("   projected ", Style::default().fg(palette.muted)),
            Span::styled(format::cost(projected), Style::default().fg(palette.cost)),
        ]);
    }
    let inner_width = area.width.saturating_sub(2) as usize;
    let first_width = first
        .iter()
        .map(|span| text_width(span.content.as_ref()))
        .sum::<usize>();
    let first = if first_width <= inner_width {
        Line::from(first)
    } else {
        let token_change = signed_tokens(token_delta);
        let compact = format!(
            "cost {} {} · tokens {} {}",
            signed_cost(cost_delta),
            percent_change(stats.totals.cost, previous.totals.cost),
            token_change,
            percent_change(
                stats.totals.total_tokens() as f64,
                previous.totals.total_tokens() as f64,
            ),
        );
        Line::from(Span::styled(
            fit_text_ellipsis(&compact, inner_width),
            Style::default().fg(palette.muted),
        ))
    };

    let second = if let Some((model, delta)) = stats.biggest_cost_increase() {
        let label = "biggest increase  ";
        let delta = signed_cost(delta);
        let model_width = inner_width
            .saturating_sub(text_width(label))
            .saturating_sub(text_width(&delta))
            .saturating_sub(2);
        Line::from(vec![
            Span::styled(label, Style::default().fg(palette.muted)),
            Span::styled(
                fit_text_ellipsis(model, model_width),
                Style::default().fg(palette.accent),
            ),
            Span::styled(format!("  {delta}"), Style::default().fg(palette.cost)),
        ])
    } else {
        Line::from(Span::styled(
            "no model increased in cost",
            Style::default().fg(palette.muted),
        ))
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Compared with previous period ")
        .title_style(Style::default().fg(palette.title))
        .border_style(Style::default().fg(palette.border));
    frame.render_widget(Paragraph::new(vec![first, second]).block(block), area);
}

fn percent_change(current: f64, previous: f64) -> String {
    if previous.abs() < f64::EPSILON {
        if current.abs() < f64::EPSILON {
            "0.0%".to_string()
        } else {
            "new".to_string()
        }
    } else {
        format!("{:+.1}%", (current - previous) / previous * 100.0)
    }
}

fn signed_cost(value: f64) -> String {
    if value >= 0.0 {
        format!("+{}", format::precise_cost(value))
    } else {
        format!("-{}", format::precise_cost(value.abs()))
    }
}

fn signed_integer(value: i128) -> String {
    if value >= 0 {
        format!("+{}", format::integer(value as u64))
    } else {
        format!("-{}", format::integer(value.unsigned_abs() as u64))
    }
}

fn signed_tokens(value: i128) -> String {
    if value >= 0 {
        format!("+{}", format::tokens(value as u64))
    } else {
        format!("-{}", format::tokens(value.unsigned_abs() as u64))
    }
}

fn delta_color(value: f64, palette: Palette) -> Color {
    if value > 0.0 {
        palette.cost
    } else if value < 0.0 {
        palette.accent
    } else {
        palette.muted
    }
}

const MODEL_TABLE_OVERHEAD: u16 = 3;
const MIN_MODEL_ROWS_WITH_GRAPH: u16 = 6;
const MIN_TOKEN_GRAPH_HEIGHT: u16 = 5;

fn can_show_token_graph(lower_area: Rect) -> bool {
    lower_area.height
        >= MODEL_TABLE_OVERHEAD
            .saturating_add(MIN_MODEL_ROWS_WITH_GRAPH)
            .saturating_add(MIN_TOKEN_GRAPH_HEIGHT)
}

fn model_graph_model_height(lower_height: u16, lower_width: u16, model_count: usize) -> u16 {
    let model_min = MODEL_TABLE_OVERHEAD.saturating_add(MIN_MODEL_ROWS_WITH_GRAPH);
    let graph_min = MIN_TOKEN_GRAPH_HEIGHT;
    let available = lower_height;
    let base = model_min.min(available.saturating_sub(graph_min));
    let remaining = available.saturating_sub(base).saturating_sub(graph_min);
    let full_model_height = if lower_width < NARROW_MODEL_WIDTH {
        2_u16.saturating_add((model_count as u16).saturating_mul(2))
    } else {
        MODEL_TABLE_OVERHEAD.saturating_add(model_count as u16)
    };
    let model_extra = remaining.min(full_model_height.saturating_sub(base));

    base.saturating_add(model_extra)
}

fn draw_calendar_overview(frame: &mut Frame<'_>, area: Rect, app: &AppState, palette: Palette) {
    let title = format!(
        " Calendar: {} | {} ",
        app.calendar.scale.title(),
        overview_title(app.calendar.scale, app.calendar.selected)
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_style(Style::default().fg(palette.title))
        .border_style(Style::default().fg(palette.border));
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    frame.render_widget(block, area);

    match app.calendar.scale {
        CalendarScale::Day => draw_day_calendar(frame, inner, app, palette),
        CalendarScale::Week => draw_period_grid(frame, inner, app, 4, palette),
        CalendarScale::Month => draw_period_grid(frame, inner, app, 3, palette),
    }
}

fn draw_day_calendar(frame: &mut Frame<'_>, area: Rect, app: &AppState, palette: Palette) {
    let selected_month = local_date(app.calendar.selected.start_millis)
        .map(|date| (date.year(), date.month()))
        .unwrap_or((0, 0));
    let max_cost = calendar_max_cost(app);

    if can_draw_framed_grid(area, 6, 7, 1) {
        draw_framed_day_calendar(frame, area, app, selected_month, max_cost, palette);
        return;
    }

    let cell_width = area.width.saturating_sub(6) / 7;
    let headers = Row::new(app.config.week_start.short_days()).style(
        Style::default()
            .fg(palette.title)
            .add_modifier(Modifier::BOLD),
    );

    let rows = app.calendar.visible_periods.chunks(7).map(|week| {
        Row::new(week.iter().map(|period| {
            let in_month = local_date(period.start_millis)
                .map(|date| (date.year(), date.month()) == selected_month)
                .unwrap_or(false);
            let cost = app.calendar_cost(*period);
            period_cell(
                *period,
                day_cell_label(*period, cost, cell_width as usize),
                cost,
                max_cost,
                app.calendar.selected == *period,
                in_month,
                palette,
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

fn draw_period_grid(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &AppState,
    columns: usize,
    palette: Palette,
) {
    let row_count = app.calendar.visible_periods.len().div_ceil(columns);
    let max_cost = calendar_max_cost(app);
    let cell_width = area.width.saturating_sub(columns.saturating_sub(1) as u16) / columns as u16;

    if can_draw_framed_grid(area, row_count, columns, 0) {
        draw_framed_period_grid(frame, area, app, columns, row_count, max_cost, palette);
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
                period_cell_label(*period, cost, app.config.week_start, cell_width as usize),
                cost,
                max_cost,
                app.calendar.selected == *period,
                true,
                palette,
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
    palette: Palette,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    draw_day_headers(
        frame,
        chunks[0],
        app.config.week_start.short_days(),
        palette,
    );
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
        palette,
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
    palette: Palette,
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
        palette,
        |_| true,
        |period, cost| period_framed_lines(period, cost, app.config.week_start),
    );
}

fn draw_framed_period_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    app: &AppState,
    grid: FramedGrid<'_>,
    palette: Palette,
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
            draw_framed_period_cell(
                frame,
                column_areas[column_idx],
                cell,
                lines(period, cost),
                palette,
            );
        }
    }
}

fn draw_day_headers(frame: &mut Frame<'_>, area: Rect, headers: [&str; 7], palette: Palette) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(even_constraints(headers.len()))
        .split(area);

    for (idx, header) in headers.iter().enumerate() {
        frame.render_widget(
            Paragraph::new(*header)
                .style(
                    Style::default()
                        .fg(palette.title)
                        .add_modifier(Modifier::BOLD),
                )
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
    palette: Palette,
) {
    let style = period_style(
        cell.period,
        cell.cost,
        cell.max_cost,
        cell.selected,
        cell.in_primary_range,
        palette,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(period_border_style(
            style,
            cell.selected,
            cell.in_primary_range,
            palette,
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

fn period_border_style(
    style: Style,
    selected: bool,
    in_primary_range: bool,
    palette: Palette,
) -> Style {
    let color = if selected {
        Color::Black
    } else if in_primary_range {
        palette.border
    } else {
        palette.muted
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

fn period_framed_lines(
    period: PeriodKey,
    cost: Option<f64>,
    week_start: time_window::WeekStart,
) -> Vec<Line<'static>> {
    vec![
        Line::from(compact_period_label(period, week_start)),
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
    palette: Palette,
) -> Cell<'static> {
    Cell::from(label).style(period_style(
        period,
        cost,
        max_cost,
        selected,
        in_primary_range,
        palette,
    ))
}

fn period_style(
    period: PeriodKey,
    cost: Option<f64>,
    max_cost: f64,
    selected: bool,
    in_primary_range: bool,
    palette: Palette,
) -> Style {
    if selected {
        return Style::default()
            .fg(Color::Black)
            .bg(palette.calendar_accent)
            .add_modifier(Modifier::BOLD);
    }

    let base = if in_primary_range {
        Style::default().fg(match period.scale {
            CalendarScale::Day => palette.text,
            CalendarScale::Week => palette.tokens,
            CalendarScale::Month => palette.cost,
        })
    } else {
        Style::default().fg(palette.muted)
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
        base.fg(fg)
            .bg(Color::Indexed(palette.heat_primary_bg[bucket]))
    } else {
        base.bg(Color::Indexed(palette.heat_dim_bg[bucket]))
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
    Some(((ratio * 6.0).round() as usize).clamp(0, 6))
}

fn day_cell_label(period: PeriodKey, cost: Option<f64>, width: usize) -> String {
    let day = local_date(period.start_millis)
        .map(|date| date.day().to_string())
        .unwrap_or_else(|| "?".to_string());
    let day = format!("{day:>2}");
    let cost_width = width.saturating_sub(text_width(&day).saturating_add(1));
    calendar_cost_label(cost, cost_width)
        .map(|cost| format!("{day} {cost}"))
        .unwrap_or(day)
}

fn period_cell_label(
    period: PeriodKey,
    cost: Option<f64>,
    week_start: time_window::WeekStart,
    width: usize,
) -> String {
    let period = compact_period_label(period, week_start);
    let cost_width = width.saturating_sub(text_width(&period).saturating_add(1));
    calendar_cost_label(cost, cost_width)
        .map(|cost| format!("{period} {cost}"))
        .unwrap_or_else(|| fit_text_ellipsis(&period, width))
}

fn calendar_cost_label(cost: Option<f64>, width: usize) -> Option<String> {
    let cost = cost?;
    let candidates = [
        format::cost(cost),
        format!("${cost:.1}"),
        format!("${cost:.0}"),
    ];
    candidates
        .into_iter()
        .find(|candidate| text_width(candidate) <= width)
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

fn compact_period_label(period: PeriodKey, week_start: time_window::WeekStart) -> String {
    let Some(start) = local_date(period.start_millis) else {
        return "period".to_string();
    };

    match period.scale {
        CalendarScale::Day => format!("{} {}", start.format("%b"), start.day()),
        CalendarScale::Week => format!(
            "W{} {}",
            time_window::week_number(start, week_start),
            start.format("%b %d")
        ),
        CalendarScale::Month => start.format("%b").to_string(),
    }
}

fn detail_period_label(period: PeriodKey, week_start: time_window::WeekStart) -> String {
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
                time_window::week_number(start, week_start),
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

fn draw_summary(
    frame: &mut Frame<'_>,
    area: Rect,
    stats: Option<&UsageStats>,
    loading: bool,
    palette: Palette,
) {
    let loading = loading && stats.is_none();
    let metric_areas = summary_metric_areas(area);

    let totals = stats.map(|stats| &stats.totals);
    let cost = totals
        .map(|totals| format::cost(totals.cost))
        .unwrap_or_else(|| "--".to_string());
    let messages = totals
        .map(|totals| {
            let inner_width = metric_areas[0].width.saturating_sub(2) as usize;
            if totals.api_estimated_messages > 0 && totals.unpriced_messages > 0 {
                if inner_width < 22 {
                    format!("{} msgs", format::integer(totals.messages))
                } else {
                    format!(
                        "{} msgs · {} est. · {} unpriced",
                        format::integer(totals.messages),
                        format::integer(totals.api_estimated_messages),
                        format::integer(totals.unpriced_messages)
                    )
                }
            } else if totals.api_estimated_messages > 0 {
                if inner_width < 22 {
                    format!(
                        "{} msgs\n{} estimated",
                        format::integer(totals.messages),
                        format::integer(totals.api_estimated_messages)
                    )
                } else {
                    format!(
                        "{} msgs · {} estimated",
                        format::integer(totals.messages),
                        format::integer(totals.api_estimated_messages)
                    )
                }
            } else if totals.unpriced_messages > 0 {
                if inner_width < 22 {
                    format!(
                        "{} msgs\n{} unpriced",
                        format::integer(totals.messages),
                        format::integer(totals.unpriced_messages)
                    )
                } else {
                    format!(
                        "{} msgs · {} unpriced",
                        format::integer(totals.messages),
                        format::integer(totals.unpriced_messages)
                    )
                }
            } else {
                format!("{} msgs", format::integer(totals.messages))
            }
        })
        .unwrap_or_else(|| metric_sub("messages", loading));
    let cost_title = if totals.is_some_and(|totals| totals.api_estimated_messages > 0) {
        if metric_areas[0].width < 24 {
            "Cost + API est."
        } else {
            "Cost incl. API Est."
        }
    } else if totals.is_some_and(|totals| totals.unpriced_messages > 0) {
        "Known Cost"
    } else {
        "Cost"
    };
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
        metric_areas[0],
        cost_title,
        &cost,
        &messages,
        MetricStyle {
            value: palette.cost,
            label: palette.muted,
        },
        palette,
    );
    draw_metric(
        frame,
        metric_areas[1],
        "Total Tokens",
        &total_tokens,
        &token_sub,
        MetricStyle {
            value: palette.tokens,
            label: palette.muted,
        },
        palette,
    );
    draw_metric(
        frame,
        metric_areas[2],
        "Input / Output",
        &input_output,
        &io_sub,
        MetricStyle {
            value: palette.io,
            label: palette.muted,
        },
        palette,
    );
    draw_metric(
        frame,
        metric_areas[3],
        "Cache",
        &cache,
        &cache_sub,
        MetricStyle {
            value: palette.cache,
            label: palette.muted,
        },
        palette,
    );
}

const STACKED_SUMMARY_WIDTH: u16 = 78;

fn summary_height(width: u16) -> u16 {
    if width < STACKED_SUMMARY_WIDTH {
        10
    } else {
        5
    }
}

fn summary_metric_areas(area: Rect) -> Vec<Rect> {
    if area.width >= STACKED_SUMMARY_WIDTH {
        return Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
            ])
            .split(area)
            .to_vec();
    }

    Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area)
        .iter()
        .flat_map(|row| {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(*row)
                .to_vec()
        })
        .collect()
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
    palette: Palette,
) {
    let title = fit_text_ellipsis(title, area.width.saturating_sub(4) as usize);
    let mut lines = vec![Line::from(Span::styled(
        value.to_string(),
        Style::default()
            .fg(style.value)
            .add_modifier(Modifier::BOLD),
    ))];
    lines.extend(sub.lines().map(|line| {
        Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(style.label),
        ))
    }));
    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} "))
                .title_style(Style::default().fg(style.value))
                .border_style(Style::default().fg(palette.border)),
        )
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}

fn draw_models(frame: &mut Frame<'_>, area: Rect, view: ModelViewState<'_>, palette: Palette) {
    let title = if view.first_sync {
        first_sync_title(view.title, view.source_sync_status)
    } else {
        view.title.to_string()
    };
    let title = fit_text_ellipsis(&title, area.width.saturating_sub(4) as usize);
    if view.first_sync
        && view
            .stats
            .map(|stats| stats.totals.messages == 0)
            .unwrap_or(true)
    {
        let paragraph = Paragraph::new(first_sync_lines(
            view.source_sync_status,
            area.width.saturating_sub(4) as usize,
            palette,
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} "))
                .title_style(Style::default().fg(palette.title))
                .border_style(Style::default().fg(palette.border)),
        )
        .alignment(Alignment::Center);
        frame.render_widget(paragraph, area);
        return;
    }
    let Some(stats) = view.stats else {
        let content = if view.loading {
            vec![Line::from("Loading usage...")]
        } else {
            vec![Line::from("No usage loaded. Press r to refresh.")]
        };
        let paragraph = Paragraph::new(content)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {title} "))
                    .title_style(Style::default().fg(palette.title))
                    .border_style(Style::default().fg(palette.border)),
            )
            .style(Style::default().fg(if view.loading {
                palette.tokens
            } else {
                palette.muted
            }))
            .alignment(Alignment::Center);
        frame.render_widget(paragraph, area);
        return;
    };

    if area.width >= 112 {
        draw_wide_models(frame, area, stats, &title, view.scroll, palette);
    } else if area.width >= NARROW_MODEL_WIDTH {
        draw_compact_models(frame, area, stats, &title, view.scroll, palette);
    } else {
        draw_narrow_models(frame, area, stats, &title, view.scroll, palette);
    }
}

fn first_sync_title(title: &str, status: Option<&SourceSyncStatus>) -> String {
    let spinner = sync_spinner();
    match status.filter(|status| !status.sources.is_empty()) {
        Some(status) => format!(
            "{title} · {spinner} first sync {}/{}",
            status.completed(),
            status.sources.len()
        ),
        None => format!("{title} · {spinner} first sync"),
    }
}

fn first_sync_lines(
    status: Option<&SourceSyncStatus>,
    available_width: usize,
    palette: Palette,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{} Performing first sync…", sync_spinner()),
            Style::default()
                .fg(palette.tokens)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    let Some(status) = status.filter(|status| !status.sources.is_empty()) else {
        lines.push(Line::from(Span::styled(
            "Preparing sources",
            Style::default().fg(palette.muted),
        )));
        return lines;
    };

    lines.extend(status.sources.iter().map(|source| {
        let (marker, detail, color) = match source.stage {
            SourceSyncStage::Pending => ("○", "waiting".to_string(), palette.muted),
            SourceSyncStage::Syncing => ("◌", "syncing…".to_string(), palette.tokens),
            SourceSyncStage::Ready { scanned, imported } => (
                "✓",
                format!("ready · {imported} indexed · {scanned} scanned"),
                palette.cache,
            ),
            SourceSyncStage::Failed => ("!", "failed".to_string(), palette.error),
        };
        let prefix = format!("{marker} {}", source.name);
        let full = format!("{prefix}  {detail}");
        let visible = if text_width(&full) <= available_width {
            full
        } else {
            fit_text_ellipsis(&prefix, available_width)
        };
        Line::from(Span::styled(visible, Style::default().fg(color)))
    }));
    lines
}

fn sync_spinner() -> &'static str {
    const FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    let frame = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| (duration.as_millis() / 120) as usize)
        .unwrap_or(0);
    FRAMES[frame % FRAMES.len()]
}

fn draw_wide_models(
    frame: &mut Frame<'_>,
    area: Rect,
    stats: &UsageStats,
    title: &str,
    scroll: usize,
    palette: Palette,
) {
    let max_cost = stats.models.first().map(model_cost).unwrap_or(0.0);
    let rows = stats
        .models
        .iter()
        .map(|model| wide_row(model, &stats.totals, max_cost, palette));

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
            .fg(palette.title)
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
            .title(format!(" {title} "))
            .title_style(Style::default().fg(palette.title))
            .border_style(Style::default().fg(palette.border)),
    )
    .column_spacing(1);

    let mut state = TableState::new()
        .with_offset(scroll.min(model_breakdown_max_scroll(area, stats.models.len())));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_compact_models(
    frame: &mut Frame<'_>,
    area: Rect,
    stats: &UsageStats,
    title: &str,
    scroll: usize,
    palette: Palette,
) {
    let max_cost = stats.models.first().map(model_cost).unwrap_or(0.0);
    let rows = stats
        .models
        .iter()
        .map(|model| compact_row(model, &stats.totals, max_cost, palette));

    let header = Row::new([
        Cell::from("Model"),
        Cell::from("Msgs"),
        Cell::from("Cost"),
        Cell::from("Tokens"),
        Cell::from("Cost %"),
    ])
    .style(
        Style::default()
            .fg(palette.title)
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
            .title(format!(" {title} "))
            .title_style(Style::default().fg(palette.title))
            .border_style(Style::default().fg(palette.border)),
    )
    .column_spacing(1);

    let mut state = TableState::new()
        .with_offset(scroll.min(model_breakdown_max_scroll(area, stats.models.len())));
    frame.render_stateful_widget(table, area, &mut state);
}

const NARROW_MODEL_WIDTH: u16 = 78;

fn draw_narrow_models(
    frame: &mut Frame<'_>,
    area: Rect,
    stats: &UsageStats,
    title: &str,
    scroll: usize,
    palette: Palette,
) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let rows = stats
        .models
        .iter()
        .map(|model| narrow_row(model, &stats.totals, inner_width, palette));
    let table = Table::new(rows, [Constraint::Min(1)]).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {title} "))
            .title_style(Style::default().fg(palette.title))
            .border_style(Style::default().fg(palette.border)),
    );
    let mut state = TableState::new()
        .with_offset(scroll.min(model_breakdown_max_scroll(area, stats.models.len())));
    frame.render_stateful_widget(table, area, &mut state);
}

fn narrow_row(
    model: &ModelUsage,
    totals: &UsageTotals,
    width: usize,
    palette: Palette,
) -> Row<'static> {
    let name = fit_text_ellipsis(&model.display_name, width);
    let messages = format::integer(model.totals.messages);
    let tokens = format::tokens(model.totals.total_tokens());
    let cost = model_cost_label(model);
    let share = format::percent(model.totals.cost, totals.cost);
    let full = format!("{messages} msgs · {tokens} tokens · {cost} · {share}");
    let compact = format!("{messages} · {tokens} · {cost} · {share}");
    let detail = if text_width(&full) <= width {
        full
    } else {
        compact
    };
    let detail = fit_text_ellipsis(&detail, width);

    Row::new([Cell::from(vec![
        Line::from(Span::styled(
            name,
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(detail, Style::default().fg(palette.muted))),
    ])])
    .height(2)
}

fn model_breakdown_visible_rows(area: Rect) -> usize {
    if area.width < NARROW_MODEL_WIDTH {
        area.height.saturating_sub(2) as usize / 2
    } else {
        area.height.saturating_sub(3) as usize
    }
}

fn wide_row(
    model: &ModelUsage,
    totals: &UsageTotals,
    max_cost: f64,
    palette: Palette,
) -> Row<'static> {
    Row::new([
        styled_cell(model.display_name.clone(), palette.accent, true),
        styled_cell(format::integer(model.totals.messages), palette.muted, false),
        styled_cell(model_cost_label(model), palette.cost, true),
        styled_cell(
            format::tokens(model.totals.total_tokens()),
            palette.tokens,
            true,
        ),
        styled_cell(format::tokens(model.totals.input), palette.io, false),
        styled_cell(format::tokens(model.totals.output), palette.io, false),
        styled_cell(
            format::tokens(model.totals.cache_read),
            palette.cache,
            false,
        ),
        styled_cell(
            format::tokens(model.totals.cache_write),
            palette.cache,
            false,
        ),
        styled_cell(
            share_cell(model.totals.cost, totals.cost, max_cost),
            palette.cost,
            false,
        ),
    ])
    .style(Style::default().fg(palette.text))
}

fn compact_row(
    model: &ModelUsage,
    totals: &UsageTotals,
    max_cost: f64,
    palette: Palette,
) -> Row<'static> {
    Row::new([
        styled_cell(model.display_name.clone(), palette.accent, true),
        styled_cell(format::integer(model.totals.messages), palette.muted, false),
        styled_cell(model_cost_label(model), palette.cost, true),
        styled_cell(
            format::tokens(model.totals.total_tokens()),
            palette.tokens,
            true,
        ),
        styled_cell(
            share_cell(model.totals.cost, totals.cost, max_cost),
            palette.cost,
            false,
        ),
    ])
    .style(Style::default().fg(palette.text))
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

fn model_cost_label(model: &ModelUsage) -> String {
    let cost = format::precise_cost(model.totals.cost);
    if model.totals.api_estimated_cost > 0.0 {
        format!("~{cost}")
    } else {
        cost
    }
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, app: &AppState, palette: Palette) {
    let (mut spans, status_color) = (
        footer_controls(app.view, area.width, palette),
        match app.view {
            View::Dashboard => dashboard_status_color(app, palette),
            View::CalendarOverview => calendar_status_color(app, palette),
            View::CalendarDetail => history_status_color(app, palette),
        },
    );
    let controls_width = spans
        .iter()
        .map(|span| text_width(span.content.as_ref()))
        .sum::<usize>();
    let status_width = (area.width as usize).saturating_sub(controls_width);
    let status = match app.view {
        View::Dashboard => dashboard_status(app, status_width),
        View::CalendarOverview => calendar_status(app, status_width),
        View::CalendarDetail => history_status(app, status_width),
    };
    spans.push(Span::styled(status, Style::default().fg(status_color)));

    let text = Line::from(spans);

    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(palette.muted)),
        area,
    );
}

fn footer_controls(view: View, width: u16, palette: Palette) -> Vec<Span<'static>> {
    const DASHBOARD_TINY: &[(&str, &str)] = &[(" ? ", ""), (" q ", " | ")];
    const CALENDAR_TINY: &[(&str, &str)] = &[(" Esc ", ""), (" q ", " | ")];
    const DASHBOARD_SMALL: &[(&str, &str)] =
        &[(" ? ", " help "), (" r ", " sync "), (" q ", " quit | ")];
    const CALENDAR_SMALL: &[(&str, &str)] = &[(" Esc ", " back "), (" q ", " quit | ")];
    const DASHBOARD_MEDIUM: &[(&str, &str)] = &[
        (" Tab ", " mode "),
        (" c ", " cal "),
        (" ? ", " help "),
        (" r ", " sync "),
        (" q ", " quit | "),
    ];
    const CALENDAR_MEDIUM: &[(&str, &str)] = &[
        (" Tab ", " scale "),
        (" hjkl ", " move "),
        (" Enter ", " open "),
        (" Esc ", " back | "),
    ];
    const DETAIL_MEDIUM: &[(&str, &str)] = &[
        (" hjkl ", " period "),
        (" Tab ", " scale "),
        (" Esc ", " back | "),
    ];
    const DASHBOARD_FULL: &[(&str, &str)] = &[
        (" Tab ", " mode "),
        (" S-Tab ", " back "),
        (" c ", " calendar "),
        (" ? ", " help "),
        (" r ", " refresh "),
        (" q ", " quit | "),
    ];
    const CALENDAR_FULL: &[(&str, &str)] = &[
        (" Tab ", " scale "),
        (" hjkl ", " move "),
        (" Enter ", " open "),
        (" ? ", " help "),
        (" Esc ", " back "),
        (" q ", " quit | "),
    ];
    const DETAIL_FULL: &[(&str, &str)] = &[
        (" h/k ", " prev "),
        (" j/l ", " next "),
        (" Tab ", " scale "),
        (" ? ", " help "),
        (" Esc ", " back "),
        (" q ", " quit | "),
    ];

    let hints = match (width, view) {
        (0..44, View::Dashboard) => DASHBOARD_TINY,
        (0..44, _) => CALENDAR_TINY,
        (44..65, View::Dashboard) => DASHBOARD_SMALL,
        (44..65, _) => CALENDAR_SMALL,
        (65..105, View::Dashboard) => DASHBOARD_MEDIUM,
        (65..105, View::CalendarOverview) => CALENDAR_MEDIUM,
        (65..105, View::CalendarDetail) => DETAIL_MEDIUM,
        (_, View::Dashboard) => DASHBOARD_FULL,
        (_, View::CalendarOverview) => CALENDAR_FULL,
        (_, View::CalendarDetail) => DETAIL_FULL,
    };
    let muted = Style::default().fg(palette.muted);
    hints
        .iter()
        .flat_map(|(key, label)| [key_span(key, palette), Span::styled(*label, muted)])
        .collect()
}

fn dashboard_status(app: &AppState, available_width: usize) -> String {
    let status = if let Some(error) = &app.error {
        format!("error: {error}")
    } else if let Some(mode) = app.source_sync {
        source_sync_status_label(app, mode, available_width)
    } else if let Some(error) = &app.source_sync_error {
        format!("source warning: {error}")
    } else if let Some(stats) = app.current_stats() {
        let cutoff = cutoff_label(stats);
        format!(
            "{} | {cutoff} | {}",
            format::timestamp(stats.refreshed_at),
            app.scope_label()
        )
    } else if app.is_current_loading() {
        "loading".to_string()
    } else {
        "idle".to_string()
    };
    fit_text_ellipsis(&status, available_width)
}

fn dashboard_status_color(app: &AppState, palette: Palette) -> Color {
    if app.error.is_some() || app.source_sync_error.is_some() {
        palette.error
    } else if app.source_sync.is_some() || app.is_current_loading() {
        palette.tokens
    } else {
        palette.muted
    }
}

fn calendar_status(app: &AppState, available_width: usize) -> String {
    let status = if let Some(error) = &app.error {
        format!("error: {error}")
    } else if let Some(mode) = app.source_sync {
        source_sync_status_label(app, mode, available_width)
    } else if let Some(error) = &app.source_sync_error {
        format!("source warning: {error}")
    } else if app.calendar_loading {
        "loading calendar".to_string()
    } else {
        format!(
            "selected {} | {}",
            detail_period_label(app.calendar.selected, app.config.week_start),
            app.scope_label()
        )
    };
    fit_text_ellipsis(&status, available_width)
}

fn calendar_status_color(app: &AppState, palette: Palette) -> Color {
    if app.error.is_some() || app.source_sync_error.is_some() {
        palette.error
    } else if app.source_sync.is_some() || app.calendar_loading {
        palette.tokens
    } else {
        palette.muted
    }
}

fn history_status(app: &AppState, available_width: usize) -> String {
    let status = if let Some(error) = &app.error {
        format!("error: {error}")
    } else if let Some(mode) = app.source_sync {
        source_sync_status_label(app, mode, available_width)
    } else if let Some(error) = &app.source_sync_error {
        format!("source warning: {error}")
    } else if let Some(stats) = app.selected_history_stats() {
        let cutoff = cutoff_label(stats);
        format!(
            "{} | {cutoff} | {}",
            format::timestamp(stats.refreshed_at),
            app.scope_label()
        )
    } else if app.is_selected_history_loading() {
        "loading".to_string()
    } else {
        "idle".to_string()
    };
    fit_text_ellipsis(&status, available_width)
}

fn history_status_color(app: &AppState, palette: Palette) -> Color {
    if app.error.is_some() || app.source_sync_error.is_some() {
        palette.error
    } else if app.source_sync.is_some() || app.is_selected_history_loading() {
        palette.tokens
    } else {
        palette.muted
    }
}

fn source_sync_label(mode: SyncMode) -> &'static str {
    match mode {
        SyncMode::Incremental => "syncing sources",
        SyncMode::Full => "rebuilding usage index",
    }
}

fn source_sync_status_label(app: &AppState, mode: SyncMode, available_width: usize) -> String {
    let label = if app.first_sync {
        format!("{} performing first sync", sync_spinner())
    } else {
        source_sync_label(mode).to_string()
    };
    let Some(status) = app
        .source_sync_status
        .as_ref()
        .filter(|status| !status.sources.is_empty())
    else {
        return fit_text_ellipsis(&label, available_width);
    };
    let active = status.active_names();
    let progress = format!(
        "{label} · {}/{} complete",
        status.completed(),
        status.sources.len()
    );
    if active.is_empty() {
        let finishing = format!(
            "{label} · finishing · {}/{} complete",
            status.completed(),
            status.sources.len()
        );
        if text_width(&finishing) <= available_width {
            return finishing;
        }
        return fit_text_ellipsis(&progress, available_width);
    }

    let with_sources = format!(
        "{label} · syncing {} · {}/{} complete",
        active.join(" + "),
        status.completed(),
        status.sources.len()
    );
    if text_width(&with_sources) <= available_width {
        with_sources
    } else {
        fit_text_ellipsis(&progress, available_width)
    }
}

fn key_span(label: &'static str, palette: Palette) -> Span<'static> {
    Span::styled(label, Style::default().fg(Color::Black).bg(palette.key_bg))
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
        config::{ColorTheme, Config, Scope, ThemeScope},
        db::TokenBucket,
        time_window::{self, DailyStart, WeekStart},
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
        let palette = palette_for(ColorTheme::Aurora);

        assert_eq!(heat_bucket(0.0, 10.0), None);
        assert_eq!(heat_bucket(10.0, 10.0), Some(6));

        let style = period_style(period, Some(2.5), 10.0, false, true, palette);
        assert!(matches!(style.bg, Some(Color::Indexed(_))));

        let dimmed = period_style(period, Some(2.5), 10.0, false, false, palette);
        assert!(matches!(dimmed.bg, Some(Color::Indexed(_))));
        assert_eq!(dimmed.fg, Some(palette.muted));
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
    fn maps_short_tab_labels_at_narrow_widths() {
        let area = Rect::new(0, 0, 40, 20);

        assert_eq!(
            tab_at_position(2, 1, area),
            Some(TabTarget::Mode(Mode::Daily))
        );
        assert_eq!(
            tab_at_position(8, 1, area),
            Some(TabTarget::Mode(Mode::Weekly))
        );
        assert_eq!(
            tab_at_position(15, 1, area),
            Some(TabTarget::Mode(Mode::Monthly))
        );
        assert_eq!(
            tab_at_position(23, 1, area),
            Some(TabTarget::Mode(Mode::AllTime))
        );
        assert_eq!(tab_at_position(35, 1, area), Some(TabTarget::Calendar));
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
    fn maps_compact_day_calendar_click_to_period() {
        let selected = time_window::current_period(
            CalendarScale::Day,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            DailyStart::default(),
            WeekStart::default(),
        )
        .unwrap();
        let app = app_with_calendar(selected);
        let area = Rect::new(0, 0, 120, 24);
        let inner = main_body_area(area).inner(Margin {
            horizontal: 1,
            vertical: 1,
        });

        assert_eq!(
            calendar_period_at_position(inner.x + 1, inner.y + 3, area, &app),
            Some(selected)
        );
        assert_eq!(
            calendar_period_at_position(inner.x + 1, inner.y, area, &app),
            None
        );
    }

    #[test]
    fn maps_framed_week_calendar_click_to_period() {
        let selected = time_window::current_period(
            CalendarScale::Week,
            Local.with_ymd_and_hms(2026, 6, 18, 10, 0, 0).unwrap(),
            DailyStart::default(),
            WeekStart::default(),
        )
        .unwrap();
        let app = app_with_calendar(selected);
        let area = Rect::new(0, 0, 120, 24);
        let inner = main_body_area(area).inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let idx = app
            .calendar
            .visible_periods
            .iter()
            .position(|period| *period == selected)
            .unwrap();
        let row_areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints(even_constraints(3))
            .split(inner);
        let column_areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(even_constraints(4))
            .split(row_areas[idx / 4]);
        let cell = column_areas[idx % 4];

        assert_eq!(
            calendar_period_at_position(
                cell.x + cell.width / 2,
                cell.y + cell.height / 2,
                area,
                &app
            ),
            Some(selected)
        );
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
    fn narrow_dashboard_reflows_large_estimates_and_model_rows() {
        let mut stats = sample_stats(Mode::Daily, Some(local_millis(2026, 6, 15, 4, 0, 0)));
        stats.totals.messages = 12_345_678;
        stats.totals.api_estimated_messages = 12_345_678;
        stats.totals.api_estimated_cost = stats.totals.cost;
        stats.models[0].display_name =
            "provider/a-model-name-that-would-crush-fixed-columns".to_string();
        stats.models[0].totals.messages = 12_345_678;
        stats.models[0].totals.api_estimated_messages = 12_345_678;
        stats.models[0].totals.api_estimated_cost = stats.models[0].totals.cost;
        let app = app_with_stats(Mode::Daily, stats);

        let output = render(&app, 60, 30);

        assert!(output.contains("Cost incl. API Est."));
        assert!(output.contains("12,345,678 msgs"));
        assert!(output.matches("12,345,678").count() >= 2);
        assert!(output.contains("estimated"));
        assert!(output.contains("provider/a-model-name-that-would-crush-fixed-columns"));
        assert!(output.contains("12,345,678 msgs · 10 tokens · ~$2.5000"));
    }

    #[test]
    fn very_narrow_dashboard_keeps_primary_values_and_all_tabs() {
        let mut stats = sample_stats(Mode::Daily, None);
        stats.totals.api_estimated_messages = 2;
        stats.totals.api_estimated_cost = stats.totals.cost;
        let app = app_with_stats(Mode::Daily, stats);

        let output = render(&app, 40, 30);

        assert!(output.contains("Day   Week   Month   All"));
        assert!(output.contains("Cal"));
        assert!(output.contains("Cost + API est."));
        assert!(output.contains("2 msgs"));
        assert!(output.contains("2 estimated"));
        assert!(output.contains("provider/gpt-test (high)"));
        assert!(output.contains("2.5000"));
    }

    #[test]
    fn renders_scrolled_dashboard_model_breakdown() {
        let stats = many_model_stats(Mode::AllTime, 8);
        let mut app = app_with_stats(Mode::AllTime, stats);
        app.dashboard_model_scroll = 3;

        let output = render(&app, 100, 16);

        assert!(output.contains("provider/model-3"));
        assert!(!output.contains("provider/model-0"));
    }

    #[test]
    fn hides_token_graph_when_model_pane_is_cramped() {
        let stats = many_model_stats(Mode::Daily, 8);
        let app = app_with_stats(Mode::Daily, stats);

        let output = render(&app, 100, 22);

        assert!(!output.contains("Token usage over time"));
    }

    #[test]
    fn renders_token_graph_when_space_allows() {
        let stats = many_model_stats(Mode::Daily, 8);
        let mut app = app_with_stats(Mode::Daily, stats);
        app.dashboard_token_bucket = Some(2);

        let output = render(&app, 100, 24);
        let selected_label =
            token_graph::token_bucket_range_label(&token_buckets(3)[2], Mode::Daily);

        assert!(output.contains("Token usage over time"));
        assert!(output.contains(&format!("{selected_label} 30")));
    }

    #[test]
    fn renders_cost_graph_without_reloading_buckets() {
        let stats = many_model_stats(Mode::Daily, 8);
        let mut app = app_with_stats(Mode::Daily, stats);
        app.graph_metric = GraphMetric::Cost;

        let output = render(&app, 100, 24);

        assert!(output.contains("Cost over time"));
        assert!(output.contains("g toggles"));
    }

    #[test]
    fn renders_period_comparison_projection_and_biggest_increase() {
        let mut stats = sample_stats(Mode::Daily, Some(local_millis(2026, 6, 15, 4, 0, 0)));
        stats.projected_cost = Some(6.0);
        stats.comparison = Some(crate::db::UsageComparison {
            start_millis: local_millis(2026, 6, 14, 4, 0, 0),
            end_millis: local_millis(2026, 6, 15, 4, 0, 0),
            totals: UsageTotals {
                messages: 1,
                unpriced_messages: 0,
                api_estimated_messages: 0,
                cost: 1.0,
                api_estimated_cost: 0.0,
                total: 10,
                input: 1,
                output: 2,
                cache_read: 3,
                cache_write: 4,
            },
            models: vec![ModelUsage {
                provider: "provider".to_string(),
                model_id: "gpt-test".to_string(),
                variant: "default".to_string(),
                display_name: "provider/gpt-test".to_string(),
                totals: UsageTotals {
                    messages: 1,
                    unpriced_messages: 0,
                    api_estimated_messages: 0,
                    cost: 0.5,
                    api_estimated_cost: 0.0,
                    total: 10,
                    input: 1,
                    output: 2,
                    cache_read: 3,
                    cache_write: 4,
                },
            }],
        });
        let mut app = app_with_stats(Mode::Daily, stats);

        let output = render(&app, 140, 30);

        assert!(!output.contains("Compared with previous period"));

        app.config.show_comparison = true;

        let output = render(&app, 140, 30);

        assert!(output.contains("Compared with previous period"));
        assert!(output.contains("projected $6.00"));
        assert!(output.contains("biggest increase"));
        assert!(output.contains("provider/gpt-test (high)"));

        let narrow = render(&app, 50, 30);
        assert!(narrow.contains("cost +$2.7500"));
        assert!(narrow.contains("tokens +100"));
        assert!(narrow.contains("provider/gpt-test"));
        assert!(narrow.contains("+$2.5000"));
    }

    #[test]
    fn renders_token_graph_loading_with_summary_data() {
        let mut stats = many_model_stats(Mode::Daily, 8);
        stats.token_buckets.clear();
        let mut app = app_with_stats(Mode::Daily, stats);
        app.dashboard_graph_load.loading.insert(Mode::Daily);

        let output = render(&app, 100, 24);

        assert!(output.contains("Daily by model"));
        assert!(output.contains("Token usage over time"));
        assert!(output.contains("Loading token graph"));
    }

    #[test]
    fn renders_first_sync_source_progress_in_daily_models_area() {
        let mut app = app_loading(Mode::Daily);
        app.first_sync = true;
        app.source_sync = Some(SyncMode::Incremental);
        app.source_sync_status = Some(SourceSyncStatus {
            sources: vec![
                crate::app::SourceSyncItem {
                    name: "OpenCode".to_string(),
                    stage: SourceSyncStage::Syncing,
                },
                crate::app::SourceSyncItem {
                    name: "Pi".to_string(),
                    stage: SourceSyncStage::Pending,
                },
            ],
        });

        let output = render(&app, 100, 24);

        assert!(output.contains("Daily by model"));
        assert!(output.contains("Performing first sync"));
        assert!(output.contains("OpenCode"));
        assert!(output.contains("syncing"));
        assert!(output.contains("Pi"));
        assert!(output.contains("waiting"));
    }

    #[test]
    fn keeps_partial_model_data_visible_during_first_sync() {
        let stats = many_model_stats(Mode::Daily, 2);
        let mut app = app_with_stats(Mode::Daily, stats);
        app.first_sync = true;
        app.source_sync = Some(SyncMode::Incremental);
        app.source_sync_status = Some(SourceSyncStatus {
            sources: vec![
                crate::app::SourceSyncItem {
                    name: "OpenCode".to_string(),
                    stage: SourceSyncStage::Ready {
                        scanned: 20,
                        imported: 10,
                    },
                },
                crate::app::SourceSyncItem {
                    name: "Pi".to_string(),
                    stage: SourceSyncStage::Syncing,
                },
            ],
        });

        let output = render(&app, 120, 24);

        assert!(output.contains("first sync 1/2"));
        assert!(output.contains("provider/model-0"));
        assert!(output.contains("syncing Pi"));
    }

    #[test]
    fn sync_status_only_names_active_sources_when_they_fit() {
        let mut app = app_loading(Mode::Daily);
        app.source_sync = Some(SyncMode::Incremental);
        app.source_sync_status = Some(SourceSyncStatus {
            sources: vec![crate::app::SourceSyncItem {
                name: "OpenCode".to_string(),
                stage: SourceSyncStage::Syncing,
            }],
        });

        let wide = source_sync_status_label(&app, SyncMode::Incremental, 80);
        let narrow = source_sync_status_label(&app, SyncMode::Incremental, 25);

        assert!(wide.contains("OpenCode"));
        assert!(!narrow.contains("OpenCode"));
        assert!(text_width(&narrow) <= 25);
    }

    #[test]
    fn token_graph_spans_available_width_with_few_buckets() {
        let stats = many_model_stats(Mode::Daily, 3);
        let app = app_with_stats(Mode::Daily, stats);
        let terminal_area = Rect::new(0, 0, 100, 24);
        let graph_area = token_graph_area(terminal_area, &app).unwrap();

        let output = render(&app, terminal_area.width, terminal_area.height);
        let inner_right = graph_area.x + graph_area.width - 2;
        let graph_body_top = graph_area.y + 1;
        let graph_body_bottom = graph_area.y + graph_area.height - 1;
        let has_right_edge_bar = output.lines().enumerate().any(|(y, line)| {
            y >= graph_body_top as usize
                && y < graph_body_bottom as usize
                && char_at(line, inner_right as usize) == Some('█')
        });

        assert!(
            has_right_edge_bar,
            "expected token graph bars to reach the right edge:\n{output}"
        );
    }

    #[test]
    fn trims_empty_all_time_token_graph_edges() {
        let buckets = vec![
            test_token_bucket(0, 0),
            test_token_bucket(1, 0),
            test_token_bucket(2, 10),
            test_token_bucket(3, 0),
            test_token_bucket(4, 20),
            test_token_bucket(5, 0),
            test_token_bucket(6, 0),
        ];

        assert_eq!(
            token_graph::visible_token_bucket_range(Mode::AllTime, &buckets),
            (1, 6)
        );
    }

    #[test]
    fn preserves_fixed_window_token_graph_edges() {
        let buckets = vec![
            test_token_bucket(0, 0),
            test_token_bucket(1, 0),
            test_token_bucket(2, 10),
            test_token_bucket(3, 0),
        ];

        for mode in [Mode::Daily, Mode::Weekly, Mode::Monthly] {
            assert_eq!(
                token_graph::visible_token_bucket_range(mode, &buckets),
                (0, buckets.len()),
                "{mode:?} should preserve its full graph window"
            );
        }
    }

    #[test]
    fn renders_loading_state_when_current_mode_is_refreshing() {
        let app = app_loading(Mode::Weekly);

        let output = render(&app, 100, 24);

        assert!(output.contains("Weekly"));
        assert!(output.contains("refreshing"));
        assert!(output.contains("Loading usage"));
        assert!(output.contains("loading"));
    }

    #[test]
    fn keeps_metric_labels_stable_while_loaded_data_refreshes() {
        let stats = many_model_stats(Mode::Weekly, 4);
        let mut app = app_with_stats(Mode::Weekly, stats);
        app.loading.insert(Mode::Weekly);

        let output = render(&app, 100, 24);

        assert!(!output.contains("refreshing"));
        assert!(output.contains("all categories"));
        assert!(output.contains("input / output"));
        assert!(output.contains("read / write"));
    }

    #[test]
    fn labels_partial_cost_totals() {
        let mut stats = sample_stats(Mode::Daily, None);
        stats.totals.unpriced_messages = 1;
        let app = app_with_stats(Mode::Daily, stats);

        let output = render(&app, 100, 24);

        assert!(output.contains("Known Cost"));
        assert!(output.contains("1 unpriced"));
    }

    #[test]
    fn labels_api_estimated_costs_and_model_rows() {
        let mut stats = sample_stats(Mode::Daily, None);
        stats.totals.api_estimated_messages = 2;
        stats.totals.api_estimated_cost = stats.totals.cost;
        stats.models[0].totals.api_estimated_messages = 1;
        stats.models[0].totals.api_estimated_cost = stats.models[0].totals.cost;
        let app = app_with_stats(Mode::Daily, stats);

        let output = render(&app, 120, 24);

        assert!(output.contains("Cost incl. API Est."));
        assert!(output.contains("2 estimated"));
        assert!(output.contains("~$2.5000"));
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
            WeekStart::default(),
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
    fn narrow_day_calendar_omits_costs_that_cannot_fit() {
        let selected = time_window::current_period(
            CalendarScale::Day,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            DailyStart::default(),
            WeekStart::default(),
        )
        .unwrap();
        let mut app = app_with_calendar(selected);
        app.calendar_costs.insert(selected, 157.56);

        let very_narrow = render(&app, 40, 20);
        let readable = render(&app, 60, 24);

        assert!(!very_narrow.contains('$'));
        assert!(readable.contains("15 $158"));
    }

    #[test]
    fn renders_framed_calendar_cells_when_large_enough() {
        let selected = time_window::current_period(
            CalendarScale::Day,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            DailyStart::default(),
            WeekStart::default(),
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
            WeekStart::default(),
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
            WeekStart::default(),
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

    #[test]
    fn renders_scrolled_calendar_detail_model_breakdown() {
        let selected = time_window::current_period(
            CalendarScale::Day,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            DailyStart::default(),
            WeekStart::default(),
        )
        .unwrap();
        let stats = many_model_stats(Mode::Daily, 8);
        let mut app = app_with_calendar(selected);
        app.view = View::CalendarDetail;
        app.history_model_scroll = 3;
        app.history_stats.insert(selected, stats);

        let output = render(&app, 100, 16);

        assert!(output.contains("provider/model-3"));
        assert!(!output.contains("provider/model-0"));
    }

    #[test]
    fn renders_help_with_config_options() {
        let mut app = app_loading(Mode::Daily);
        app.show_help = true;

        let output = render(&app, 120, 40);

        assert!(output.contains("Help"));
        assert!(output.contains("Config"));
        assert!(output.contains("daily_start"));
        assert!(output.contains("refresh_seconds"));
        assert!(output.contains("week_start"));
        assert!(output.contains("show_comparison"));
        assert!(output.contains("estimate_api_cost"));
        assert!(output.contains("hidden_providers"));
        assert!(output.contains("hidden"));
        assert!(output.contains("[x]"));
        assert!(output.contains("Space / Enter"));
    }

    #[test]
    fn renders_project_scope_picker() {
        let mut app = app_loading(Mode::Daily);
        app.projects = vec![crate::db::ProjectInfo {
            id: "project-a".to_string(),
            name: "Project A".to_string(),
            worktree: "/work/project-a".to_string(),
        }];
        app.scope_picker = Some(crate::app::ScopePickerState { selection: 2 });

        let output = render(&app, 100, 24);

        assert!(output.contains("Project scope"));
        assert!(output.contains("All projects"));
        assert!(output.contains("Current directory"));
        assert!(output.contains("Project A"));
        assert!(output.contains("/work/project-a"));
    }

    #[test]
    fn renders_provider_visibility_picker() {
        let mut app = app_loading(Mode::Daily);
        app.providers = vec!["openai".to_string(), "openrouter".to_string()];
        app.config.hidden_providers.insert("openrouter".to_string());
        app.provider_picker = Some(crate::app::ProviderPickerState { selection: 1 });

        let output = render(&app, 100, 24);

        assert!(output.contains("Provider visibility"));
        assert!(output.contains("[x] openai"));
        assert!(output.contains("[ ] openrouter"));
        assert!(output.contains("toggle"));
    }

    #[test]
    fn renders_alias_editor_and_text_input() {
        let mut app = app_loading(Mode::Daily);
        app.config
            .aliases
            .providers
            .insert("github-copilot".to_string(), "gc".to_string());
        app.alias_editor = Some(crate::app::AliasEditorState {
            kind: crate::app::AliasKind::Provider,
            selection: 0,
            input: None,
        });

        let list = render(&app, 100, 24);
        assert!(list.contains("Provider aliases"));
        assert!(list.contains("github-copilot"));
        assert!(list.contains("gc"));
        assert!(list.contains("+ add alias"));

        app.alias_editor.as_mut().unwrap().input = Some(crate::app::AliasInputState {
            original_source: None,
            source: "github-copilot".to_string(),
            alias: "gc".to_string(),
            field: crate::app::AliasInputField::Alias,
        });
        let input = render(&app, 100, 24);
        assert!(input.contains("Provider ID"));
        assert!(input.contains("Display alias"));
        assert!(input.contains("next/save"));
    }

    #[test]
    fn aligns_help_binding_descriptions_to_one_column() {
        let mut app = app_loading(Mode::Daily);
        app.show_help = true;

        let output = render(&app, 120, 32);
        let descriptions = [
            "switch dashboard window or calendar scale",
            "open Calendar from the dashboard",
            "move Calendar selection",
            "open selected Calendar period",
            "back one level; close help",
            "refresh current view",
            "select a config row",
            "toggle or cycle the selected value",
            "cycle choices",
        ];
        let starts = descriptions
            .iter()
            .map(|description| description_start(&output, description))
            .collect::<Vec<_>>();

        assert!(
            starts.iter().all(|start| *start == starts[0]),
            "description starts were not aligned: {starts:?}"
        );
    }

    #[test]
    fn aligns_config_editor_values_to_one_column() {
        let mut app = app_loading(Mode::Daily);
        app.show_help = true;

        let output = render(&app, 120, 32);
        let starts = [
            config_value_start(&output, "auto_refresh", "[x]"),
            config_value_start(&output, "daily_start", "04:00"),
            config_value_start(&output, "refresh_seconds", "60s"),
            config_value_start(&output, "week_start", "monday"),
        ];

        assert!(
            starts.iter().all(|start| *start == starts[0]),
            "config value starts were not aligned: {starts:?}"
        );
    }

    #[test]
    fn renders_scrolled_config_editor_values_aligned() {
        let mut app = app_loading(Mode::Daily);
        app.show_help = true;
        app.config_selection = 5;
        let layout = help_layout_state(Rect::new(0, 0, 120, 24), &app);
        app.help_scroll = layout.config_start;

        let output = render(&app, 120, 24);
        let starts = [
            config_value_start(&output, "color_theme", "aurora"),
            config_value_start(&output, "theme_scope", "calendar"),
        ];

        assert!(
            starts.iter().all(|start| *start == starts[0]),
            "config value starts were not aligned: {starts:?}"
        );
    }

    #[test]
    fn small_help_popup_starts_at_controls() {
        let mut app = app_loading(Mode::Daily);
        app.show_help = true;

        let output = render(&app, 80, 16);

        assert!(output.contains("Controls"));
        assert!(output.contains("switch dashboard window"));
        assert!(output.contains("refresh current view"));
        assert!(!output.contains("auto_refresh"));
    }

    #[test]
    fn small_help_popup_scrolls_to_config_rows() {
        let mut app = app_loading(Mode::Daily);
        app.show_help = true;
        app.config_selection = 5;
        let layout = help_layout_state(Rect::new(0, 0, 80, 16), &app);
        app.help_scroll = layout
            .config_item_starts
            .get(app.config_selection)
            .copied()
            .unwrap();

        let output = render(&app, 80, 16);

        assert!(output.contains("theme_scope"));
        assert!(output.contains("calendar"));
        assert!(!output.contains("Controls"));
    }

    #[test]
    fn wraps_config_options_to_value_column() {
        let mut app = app_loading(Mode::Daily);
        app.show_help = true;
        let layout = help_layout_state(Rect::new(0, 0, 60, 16), &app);
        app.help_scroll = layout
            .config_item_starts
            .get(4)
            .copied()
            .unwrap()
            .saturating_sub(1);

        let output = render(&app, 60, 16);
        let first_start = config_value_start(&output, "color_theme", "aurora");
        let wrapped_start = text_start(&output, "forest");

        assert_eq!(
            wrapped_start, first_start,
            "wrapped config value started at {wrapped_start}, expected {first_start}"
        );
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

    fn description_start(output: &str, description: &str) -> usize {
        output
            .lines()
            .find_map(|line| {
                line.find(description)
                    .map(|byte_idx| line[..byte_idx].chars().count())
            })
            .unwrap_or_else(|| panic!("missing help description {description:?}"))
    }

    fn text_start(output: &str, text: &str) -> usize {
        output
            .lines()
            .find_map(|line| {
                line.find(text)
                    .map(|byte_idx| line[..byte_idx].chars().count())
            })
            .unwrap_or_else(|| panic!("missing text {text:?}"))
    }

    fn char_at(line: &str, column: usize) -> Option<char> {
        line.chars().nth(column)
    }

    fn config_value_start(output: &str, label: &str, value: &str) -> usize {
        let line = output
            .lines()
            .find(|line| line.contains(label))
            .unwrap_or_else(|| panic!("missing config label {label:?}"));
        line.find(value)
            .map(|byte_idx| line[..byte_idx].chars().count())
            .unwrap_or_else(|| panic!("missing config value {value:?} for {label:?}"))
    }

    fn app_with_stats(mode: Mode, stats: UsageStats) -> AppState {
        let mut stats_by_mode = HashMap::new();
        stats_by_mode.insert(mode, stats);

        AppState {
            config: test_config(),
            view: View::Dashboard,
            show_help: false,
            scope_picker: None,
            provider_picker: None,
            alias_editor: None,
            projects: Vec::new(),
            providers: Vec::new(),
            help_scroll: 0,
            dashboard_model_scroll: 0,
            history_model_scroll: 0,
            dashboard_token_bucket: None,
            history_token_bucket: None,
            token_graph_dragging: false,
            graph_metric: crate::app::GraphMetric::Tokens,
            config_selection: 0,
            config_notice: None,
            mode,
            stats: stats_by_mode,
            loading: HashSet::new(),
            dashboard_graph_load: crate::app::DeferredLoadState::default(),
            dashboard_prefetch_attempted: HashSet::new(),
            calendar: test_calendar(),
            calendar_costs: HashMap::new(),
            calendar_loading: false,
            history_stats: HashMap::new(),
            history_loading: HashSet::new(),
            history_graph_load: crate::app::DeferredLoadState::default(),
            error: None,
            source_sync: None,
            source_sync_status: None,
            source_sync_error: None,
            first_sync: false,
            logical_day: test_calendar().selected,
            pending_source_sync: None,
            last_refresh_started: None,
            next_refresh_due: Instant::now() + Duration::from_secs(60),
            request_generation: 0,
        }
    }

    fn app_loading(mode: Mode) -> AppState {
        AppState {
            config: test_config(),
            view: View::Dashboard,
            show_help: false,
            scope_picker: None,
            provider_picker: None,
            alias_editor: None,
            projects: Vec::new(),
            providers: Vec::new(),
            help_scroll: 0,
            dashboard_model_scroll: 0,
            history_model_scroll: 0,
            dashboard_token_bucket: None,
            history_token_bucket: None,
            token_graph_dragging: false,
            graph_metric: crate::app::GraphMetric::Tokens,
            config_selection: 0,
            config_notice: None,
            mode,
            stats: HashMap::new(),
            loading: HashSet::from([mode]),
            dashboard_graph_load: crate::app::DeferredLoadState::default(),
            dashboard_prefetch_attempted: HashSet::new(),
            calendar: test_calendar(),
            calendar_costs: HashMap::new(),
            calendar_loading: false,
            history_stats: HashMap::new(),
            history_loading: HashSet::new(),
            history_graph_load: crate::app::DeferredLoadState::default(),
            error: None,
            source_sync: None,
            source_sync_status: None,
            source_sync_error: None,
            first_sync: false,
            logical_day: test_calendar().selected,
            pending_source_sync: None,
            last_refresh_started: None,
            next_refresh_due: Instant::now() + Duration::from_secs(60),
            request_generation: 0,
        }
    }

    fn app_with_calendar(selected: PeriodKey) -> AppState {
        AppState {
            config: test_config(),
            view: View::CalendarOverview,
            show_help: false,
            scope_picker: None,
            provider_picker: None,
            alias_editor: None,
            projects: Vec::new(),
            providers: Vec::new(),
            help_scroll: 0,
            dashboard_model_scroll: 0,
            history_model_scroll: 0,
            dashboard_token_bucket: None,
            history_token_bucket: None,
            token_graph_dragging: false,
            graph_metric: crate::app::GraphMetric::Tokens,
            config_selection: 0,
            config_notice: None,
            mode: Mode::Daily,
            stats: HashMap::new(),
            loading: HashSet::new(),
            dashboard_graph_load: crate::app::DeferredLoadState::default(),
            dashboard_prefetch_attempted: HashSet::new(),
            calendar: CalendarState {
                scale: selected.scale,
                selected,
                visible_periods: time_window::visible_periods(
                    selected,
                    DailyStart::default(),
                    WeekStart::default(),
                )
                .unwrap(),
            },
            calendar_costs: HashMap::new(),
            calendar_loading: false,
            history_stats: HashMap::new(),
            history_loading: HashSet::new(),
            history_graph_load: crate::app::DeferredLoadState::default(),
            error: None,
            source_sync: None,
            source_sync_status: None,
            source_sync_error: None,
            first_sync: false,
            logical_day: selected,
            pending_source_sync: None,
            last_refresh_started: None,
            next_refresh_due: Instant::now() + Duration::from_secs(60),
            request_generation: 0,
        }
    }

    fn test_calendar() -> CalendarState {
        let selected = time_window::current_period(
            CalendarScale::Day,
            Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            DailyStart::default(),
            WeekStart::default(),
        )
        .unwrap();

        CalendarState {
            scale: CalendarScale::Day,
            selected,
            visible_periods: time_window::visible_periods(
                selected,
                DailyStart::default(),
                WeekStart::default(),
            )
            .unwrap(),
        }
    }

    fn test_config() -> Config {
        Config {
            db_path: PathBuf::from("/tmp/opencode.db"),
            index_path: PathBuf::from("/tmp/expensive.sqlite3"),
            copilot_home: PathBuf::from("/tmp/copilot"),
            codex_home: PathBuf::from("/tmp/codex"),
            pi_sessions_root: PathBuf::from("/tmp/pi/sessions"),
            current_directory: PathBuf::from("/tmp/project"),
            config_path: Some(PathBuf::from("/tmp/expensive/config.toml")),
            daily_start: DailyStart::default(),
            week_start: WeekStart::default(),
            refresh_interval: Duration::from_secs(60),
            auto_refresh: true,
            show_comparison: false,
            estimate_api_cost: false,
            hidden_providers: std::collections::BTreeSet::new(),
            scope: Scope::All,
            color_theme: ColorTheme::Aurora,
            theme_scope: ThemeScope::Calendar,
            aliases: crate::config::ModelAliases::default(),
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
                unpriced_messages: 0,
                api_estimated_messages: 0,
                cost: 2.5,
                api_estimated_cost: 0.0,
                total: 10,
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
                unpriced_messages: 0,
                api_estimated_messages: 0,
                cost: 1.25,
                api_estimated_cost: 0.0,
                total: 100,
                input: 10,
                output: 20,
                cache_read: 30,
                cache_write: 40,
            },
        };

        UsageStats {
            mode,
            refreshed_at: Local.with_ymd_and_hms(2026, 6, 15, 10, 0, 0).unwrap(),
            snapshot_millis: 1_750_000_000_000,
            cutoff_millis,
            end_millis: None,
            totals: UsageTotals {
                messages: 2,
                unpriced_messages: 0,
                api_estimated_messages: 0,
                cost: 3.75,
                api_estimated_cost: 0.0,
                total: 110,
                input: 11,
                output: 22,
                cache_read: 33,
                cache_write: 44,
            },
            models: vec![high, default],
            token_buckets: token_buckets(4),
            comparison: None,
            projected_cost: None,
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
                    unpriced_messages: 0,
                    api_estimated_messages: 0,
                    cost: (count - idx) as f64,
                    api_estimated_cost: 0.0,
                    total: 100,
                    input: 10,
                    output: 20,
                    cache_read: 30,
                    cache_write: 40,
                },
            })
            .collect::<Vec<_>>();
        let totals = UsageTotals {
            messages: count as u64,
            unpriced_messages: 0,
            api_estimated_messages: 0,
            cost: models.iter().map(|model| model.totals.cost).sum(),
            api_estimated_cost: 0.0,
            total: count as u64 * 100,
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
            comparison: None,
            projected_cost: None,
        }
    }

    fn token_buckets(count: usize) -> Vec<TokenBucket> {
        (0..count)
            .map(|idx| test_token_bucket(idx, (idx as u64 + 1) * 10))
            .collect()
    }

    fn test_token_bucket(idx: usize, tokens: u64) -> TokenBucket {
        const HOUR: i64 = 60 * 60 * 1000;
        TokenBucket {
            start_millis: idx as i64 * 2 * HOUR,
            end_millis: (idx as i64 + 1) * 2 * HOUR,
            tokens,
            cost: tokens as f64 / 100.0,
            api_estimated_cost: 0.0,
        }
    }

    fn local_millis(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, second)
            .unwrap()
            .timestamp_millis()
    }
}
