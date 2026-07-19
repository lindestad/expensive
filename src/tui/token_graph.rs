use ratatui::{
    layout::{Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{
    app::GraphMetric,
    db::{TokenBucket, UsageStats},
    format,
    time_window::Mode,
};

use super::{local_date, rect_contains, Palette};

#[derive(Clone, Copy)]
struct GraphBucket {
    start_idx: usize,
    end_idx: usize,
    start_millis: i64,
    end_millis: i64,
    tokens: u64,
    cost: f64,
}

#[derive(Clone, Copy)]
struct GraphColumn {
    bucket: GraphBucket,
    visible: bool,
}

pub(super) fn draw_token_graph(
    frame: &mut Frame<'_>,
    area: Rect,
    stats: &UsageStats,
    selected_bucket: Option<usize>,
    metric: GraphMetric,
    palette: Palette,
) {
    let inner = token_graph_inner_area(area);
    let (visible_start, visible_end) = visible_token_bucket_range(stats.mode, &stats.token_buckets);
    let columns = token_graph_columns(
        &stats.token_buckets[visible_start..visible_end],
        inner.width as usize,
        visible_start,
    );
    let max_value = columns
        .iter()
        .map(|column| graph_bucket_value(column.bucket, metric))
        .fold(0.0_f64, f64::max);
    let axis_height = usize::from(inner.height > 1);
    let graph_height = (inner.height as usize).saturating_sub(axis_height).max(1);
    let selected_bucket = selected_bucket.filter(|idx| *idx < stats.token_buckets.len());

    let mut lines = (0..graph_height)
        .map(|row| {
            let level = graph_height.saturating_sub(row);
            let spans = columns
                .iter()
                .map(|column| {
                    let bucket = column.bucket;
                    let value = graph_bucket_value(bucket, metric);
                    let filled_height = if max_value <= 0.0 || value <= 0.0 {
                        0
                    } else {
                        ((value / max_value) * graph_height as f64).ceil() as usize
                    };
                    let selected = selected_bucket
                        .map(|idx| idx >= bucket.start_idx && idx < bucket.end_idx)
                        .unwrap_or(false);
                    let color = if selected {
                        palette.calendar_accent
                    } else if value <= 0.0 {
                        palette.muted
                    } else {
                        match metric {
                            GraphMetric::Tokens => palette.tokens,
                            GraphMetric::Cost => palette.cost,
                        }
                    };
                    let mut style = Style::default().fg(color);
                    if selected {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    Span::styled(
                        if column.visible && filled_height >= level {
                            "█"
                        } else {
                            " "
                        },
                        style,
                    )
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    if axis_height > 0 {
        lines.push(token_graph_axis_line(&columns, stats.mode, palette));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(token_graph_title(stats, selected_bucket, metric))
        .title_style(Style::default().fg(palette.title))
        .border_style(Style::default().fg(palette.border));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

pub(super) fn draw_token_graph_loading(
    frame: &mut Frame<'_>,
    area: Rect,
    metric: GraphMetric,
    palette: Palette,
) {
    let label = match metric {
        GraphMetric::Tokens => "Token usage",
        GraphMetric::Cost => "Cost",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {label} over time "))
        .title_style(Style::default().fg(palette.title))
        .border_style(Style::default().fg(palette.border));
    let loading = match metric {
        GraphMetric::Tokens => "Loading token graph...",
        GraphMetric::Cost => "Loading cost graph...",
    };
    let paragraph = Paragraph::new(loading)
        .block(block)
        .style(Style::default().fg(palette.tokens));

    frame.render_widget(paragraph, area);
}

fn token_graph_title(
    stats: &UsageStats,
    selected_bucket: Option<usize>,
    metric: GraphMetric,
) -> String {
    let (label, value) = match metric {
        GraphMetric::Tokens => (
            "Token usage",
            selected_bucket
                .and_then(|idx| stats.token_buckets.get(idx))
                .map(|bucket| format::tokens(bucket.tokens)),
        ),
        GraphMetric::Cost => (
            "Cost",
            selected_bucket
                .and_then(|idx| stats.token_buckets.get(idx))
                .map(|bucket| format::precise_cost(bucket.cost)),
        ),
    };
    if let Some(bucket) = selected_bucket.and_then(|idx| stats.token_buckets.get(idx)) {
        return format!(
            " {label} over time | {} {} ",
            token_bucket_range_label(bucket, stats.mode),
            value.unwrap_or_default()
        );
    }

    format!(" {label} over time | g toggles ")
}

fn graph_bucket_value(bucket: GraphBucket, metric: GraphMetric) -> f64 {
    match metric {
        GraphMetric::Tokens => bucket.tokens as f64,
        GraphMetric::Cost => bucket.cost,
    }
}

fn token_graph_axis_line(columns: &[GraphColumn], mode: Mode, palette: Palette) -> Line<'static> {
    if columns.is_empty() {
        return Line::from("");
    }

    let mut cells = vec![' '; columns.len()];
    let tick_count = if columns.len() >= 36 { 4 } else { 3 }.min(columns.len());
    for tick_idx in 0..tick_count {
        let column_idx = if tick_count == 1 {
            0
        } else {
            tick_idx * (columns.len() - 1) / (tick_count - 1)
        };
        let label = token_axis_label(&columns[column_idx].bucket, mode);
        place_axis_label(&mut cells, column_idx, &label);
    }

    Line::from(Span::styled(
        cells.into_iter().collect::<String>(),
        Style::default().fg(palette.muted),
    ))
}

fn place_axis_label(cells: &mut [char], center: usize, label: &str) {
    if cells.is_empty() || label.is_empty() {
        return;
    }

    let label_width = label.chars().count();
    if label_width > cells.len() {
        return;
    }

    let start = center
        .saturating_sub(label_width / 2)
        .min(cells.len().saturating_sub(label_width));
    if cells[start..start + label_width]
        .iter()
        .any(|value| *value != ' ')
    {
        return;
    }

    for (idx, ch) in label.chars().enumerate() {
        cells[start + idx] = ch;
    }
}

fn token_axis_label(bucket: &GraphBucket, mode: Mode) -> String {
    let Some(start) = local_date(bucket.start_millis) else {
        return String::new();
    };

    match mode {
        Mode::Daily => start.format("%H").to_string(),
        Mode::Weekly | Mode::Monthly => start.format("%b %d").to_string(),
        Mode::AllTime => {
            let span = bucket.end_millis.saturating_sub(bucket.start_millis);
            if span <= 7 * 24 * 60 * 60 * 1000 {
                start.format("%b %d").to_string()
            } else {
                start.format("%b").to_string()
            }
        }
    }
}

pub(super) fn token_bucket_range_label(bucket: &TokenBucket, mode: Mode) -> String {
    let Some(start) = local_date(bucket.start_millis) else {
        return "bucket".to_string();
    };
    let end = local_date(bucket.end_millis);

    match mode {
        Mode::Daily => end
            .map(|end| format!("{}-{}", start.format("%H:%M"), end.format("%H:%M")))
            .unwrap_or_else(|| start.format("%H:%M").to_string()),
        Mode::Weekly | Mode::Monthly => start.format("%b %d").to_string(),
        Mode::AllTime => {
            let span = bucket.end_millis.saturating_sub(bucket.start_millis);
            if span <= 7 * 24 * 60 * 60 * 1000 {
                start.format("%b %d").to_string()
            } else {
                start.format("%b %Y").to_string()
            }
        }
    }
}

pub(super) fn token_bucket_index_at_position(
    column: u16,
    row: u16,
    area: Rect,
    stats: &UsageStats,
) -> Option<usize> {
    if !rect_contains(area, column, row) {
        return None;
    }

    let inner = token_graph_inner_area(area);
    if inner.width == 0 || inner.height == 0 {
        return None;
    }

    let (visible_start, visible_end) = visible_token_bucket_range(stats.mode, &stats.token_buckets);
    let columns = token_graph_columns(
        &stats.token_buckets[visible_start..visible_end],
        inner.width as usize,
        visible_start,
    );
    if columns.is_empty() {
        return None;
    }

    let max_column_idx = columns.len().saturating_sub(1);
    let column_idx = if column < inner.x {
        0
    } else if column >= inner.x.saturating_add(inner.width) {
        max_column_idx
    } else {
        (column.checked_sub(inner.x)? as usize).min(max_column_idx)
    };
    columns
        .get(column_idx)
        .map(|column| column.bucket.start_idx)
}

fn token_graph_inner_area(area: Rect) -> Rect {
    area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    })
}

pub(super) fn visible_token_bucket_range(mode: Mode, buckets: &[TokenBucket]) -> (usize, usize) {
    if buckets.is_empty() {
        return (0, 0);
    }

    if matches!(mode, Mode::Daily | Mode::Weekly | Mode::Monthly) {
        return (0, buckets.len());
    }

    let Some(first_used) = buckets.iter().position(|bucket| bucket.tokens > 0) else {
        return (0, buckets.len());
    };
    let last_used = buckets
        .iter()
        .rposition(|bucket| bucket.tokens > 0)
        .unwrap_or(first_used);

    (
        first_used.saturating_sub(1),
        last_used.saturating_add(2).min(buckets.len()),
    )
}

fn token_graph_columns(
    buckets: &[TokenBucket],
    width: usize,
    start_offset: usize,
) -> Vec<GraphColumn> {
    if width == 0 || buckets.is_empty() {
        return Vec::new();
    }

    let show_separators = buckets.len() < width && width / buckets.len() >= 3;
    let mut previous_start_idx = None;

    (0..width)
        .map(|column_idx| {
            let bucket = graph_bucket_for_column(buckets, width, start_offset, column_idx);
            let visible = !(show_separators
                && previous_start_idx
                    .map(|idx| idx != bucket.start_idx)
                    .unwrap_or(false));
            previous_start_idx = Some(bucket.start_idx);

            GraphColumn { bucket, visible }
        })
        .collect()
}

fn graph_bucket_for_column(
    buckets: &[TokenBucket],
    width: usize,
    start_offset: usize,
    column_idx: usize,
) -> GraphBucket {
    if buckets.len() < width {
        let bucket_idx = column_idx * buckets.len() / width;
        return GraphBucket {
            start_idx: start_offset + bucket_idx,
            end_idx: start_offset + bucket_idx + 1,
            start_millis: buckets[bucket_idx].start_millis,
            end_millis: buckets[bucket_idx].end_millis,
            tokens: buckets[bucket_idx].tokens,
            cost: buckets[bucket_idx].cost,
        };
    }

    let start_idx = column_idx * buckets.len() / width;
    let end_idx = ((column_idx + 1) * buckets.len() / width).max(start_idx + 1);
    let tokens = buckets[start_idx..end_idx]
        .iter()
        .fold(0_u64, |total, bucket| total.saturating_add(bucket.tokens));
    let cost = buckets[start_idx..end_idx]
        .iter()
        .map(|bucket| bucket.cost)
        .sum();

    GraphBucket {
        start_idx: start_offset + start_idx,
        end_idx: start_offset + end_idx,
        start_millis: buckets[start_idx].start_millis,
        end_millis: buckets[end_idx - 1].end_millis,
        tokens,
        cost,
    }
}
