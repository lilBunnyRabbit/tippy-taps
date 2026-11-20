use eframe::egui::{self, Align2, CornerRadius, FontId, Rect, Sense, Stroke, pos2, vec2};

use super::{
    BRAND_BG_LIGHT, BRAND_BG_LIGHTEST, BRAND_COTTONTAIL, BRAND_DARKER, BRAND_FG, BRAND_FG_WEAK,
    MONTHS, SERIES, WEEKDAYS, heat,
};
use crate::clock::Date;
use crate::counts::Bucket;
use crate::stats::DAY_START;

pub const AXIS_WIDTH: f32 = 40.0;
const AXIS_HEIGHT: f32 = 18.0;
const SEGMENT_GAP: f32 = 2.0;

pub fn small() -> FontId {
    FontId::proportional(11.0)
}

pub fn compact(n: f32) -> String {
    match n {
        n if n >= 1e6 => format!("{:.1}M", n / 1e6),
        n if n >= 1e4 => format!("{:.0}k", n / 1e3),
        n if n >= 1e3 => format!("{:.1}k", n / 1e3),
        n => format!("{n:.0}"),
    }
}

fn nice_step(max: f32) -> f32 {
    if max <= 0.0 {
        return 1.0;
    }
    let raw = max / 3.0;
    let mag = 10f32.powf(raw.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag)
}

pub fn legend(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (color, name) in SERIES.iter().zip(["keys", "clicks", "scrolls"]) {
            let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, *color);
            ui.label(egui::RichText::new(name).small().color(BRAND_FG_WEAK));
            ui.add_space(6.0);
        }
    });
}

fn stack(b: &Bucket) -> [f32; 3] {
    [b.keys as f32, b.clicks as f32, b.scrolls as f32]
}

fn stacked_bar(painter: &egui::Painter, x: f32, w: f32, base: f32, scale: f32, parts: [f32; 3]) {
    let top_index = parts.iter().rposition(|v| *v > 0.0);
    let gap = if w >= 4.0 { SEGMENT_GAP } else { 0.0 };
    let mut y = base;
    for (i, v) in parts.iter().enumerate() {
        let h = v * scale;
        if h <= 0.0 {
            continue;
        }
        let r = Rect::from_min_max(pos2(x, y - h), pos2(x + w, y));
        let r = if h > gap + 1.0 && Some(i) != top_index {
            Rect::from_min_max(pos2(r.min.x, r.min.y + gap), r.max)
        } else {
            r
        };
        let round = if Some(i) == top_index {
            let c = (w / 2.0).min(3.0) as u8;
            CornerRadius {
                nw: c,
                ne: c,
                sw: 0,
                se: 0,
            }
        } else {
            CornerRadius::ZERO
        };
        painter.rect_filled(r, round, SERIES[i]);
        y -= h;
    }
}

fn y_axis(painter: &egui::Painter, plot: Rect, max: f32, step: f32, label: &dyn Fn(f32) -> String) {
    let mut v = step;
    while v <= max * 1.0001 {
        let y = plot.bottom() - v / max * plot.height();
        painter.line_segment(
            [pos2(plot.left(), y), pos2(plot.right(), y)],
            Stroke::new(1.0, BRAND_BG_LIGHTEST),
        );
        painter.text(
            pos2(plot.left() - 8.0, y),
            Align2::RIGHT_CENTER,
            label(v),
            small(),
            BRAND_FG_WEAK,
        );
        v += step;
    }
    painter.line_segment(
        [plot.left_bottom(), plot.right_bottom()],
        Stroke::new(1.0, BRAND_BG_LIGHTEST),
    );
}

fn x_labels(painter: &egui::Painter, plot: Rect, start: Date, n: usize) {
    let slot = plot.width() / n as f32;
    let mut last_x = f32::MIN;
    for i in 0..n {
        let date = start.plus_days(i as i64);
        let text = if n <= 45 {
            (date.weekday() == 0)
                .then(|| format!("{} {}", date.day, MONTHS[date.month as usize - 1]))
        } else {
            (date.day == 1).then(|| MONTHS[date.month as usize - 1].to_string())
        };
        let x = plot.left() + i as f32 * slot;
        if let Some(text) = text.filter(|_| x - last_x > 36.0 && x < plot.right() - 20.0) {
            painter.text(
                pos2(x, plot.bottom() + 4.0),
                Align2::LEFT_TOP,
                text,
                small(),
                BRAND_FG_WEAK,
            );
            last_x = x;
        }
    }
}

fn hovered_index(response: &egui::Response, plot: Rect, n: usize) -> Option<usize> {
    let p = response.hover_pos()?;
    (p.x >= plot.left() && p.x < plot.right())
        .then(|| (((p.x - plot.left()) / plot.width() * n as f32) as usize).min(n - 1))
}

fn highlight_column(painter: &egui::Painter, plot: Rect, n: usize, i: Option<usize>) {
    if let Some(i) = i {
        let slot = plot.width() / n as f32;
        let x = plot.left() + i as f32 * slot;
        painter.rect_filled(
            Rect::from_min_max(
                pos2(x - 1.0, plot.top()),
                pos2(x + slot + 1.0, plot.bottom()),
            ),
            2.0,
            BRAND_BG_LIGHT.lerp_to_gamma(BRAND_BG_LIGHTEST, 0.8),
        );
    }
}

pub fn trend(
    ui: &mut egui::Ui,
    height: f32,
    days: &[Bucket],
    avg: &[f32],
    start: Date,
) -> Option<usize> {
    let n = days.len().max(1);
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let plot = Rect::from_min_max(
        pos2(rect.left() + AXIS_WIDTH, rect.top() + 6.0),
        pos2(rect.right(), rect.bottom() - AXIS_HEIGHT),
    );
    let hovered = hovered_index(&response, plot, n);
    let painter = ui.painter_at(rect);
    let peak = days
        .iter()
        .map(|b| stack(b).iter().sum::<f32>())
        .chain(avg.iter().copied())
        .fold(0.0, f32::max);
    let step = nice_step(peak);
    let max = (peak / step).ceil().max(1.0) * step;
    highlight_column(&painter, plot, n, hovered);
    y_axis(&painter, plot, max, step, &|v| compact(v));
    let slot = plot.width() / n as f32;
    let gap = if slot >= 6.0 {
        2.0
    } else if slot >= 3.0 {
        1.0
    } else {
        0.0
    };
    let scale = plot.height() / max;
    for (i, b) in days.iter().enumerate() {
        let x = plot.left() + i as f32 * slot + gap / 2.0;
        stacked_bar(
            &painter,
            x,
            slot - gap,
            plot.bottom() - 1.0,
            scale,
            stack(b),
        );
    }
    let points: Vec<_> = avg
        .iter()
        .enumerate()
        .map(|(i, v)| {
            pos2(
                plot.left() + (i as f32 + 0.5) * slot,
                plot.bottom() - v * scale,
            )
        })
        .collect();
    painter.line(points, Stroke::new(2.0, BRAND_COTTONTAIL));
    x_labels(&painter, plot, start, n);
    hovered
}

pub fn weekday_bars(ui: &mut egui::Ui, height: f32, weekdays: &[[f32; 3]; 7]) -> Option<usize> {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let plot = Rect::from_min_max(
        pos2(rect.left() + AXIS_WIDTH, rect.top() + 6.0),
        pos2(rect.right(), rect.bottom() - AXIS_HEIGHT),
    );
    let hovered = hovered_index(&response, plot, 7);
    let painter = ui.painter_at(rect);
    let peak = weekdays
        .iter()
        .map(|w| w.iter().sum::<f32>())
        .fold(0.0, f32::max);
    let step = nice_step(peak);
    let max = (peak / step).ceil().max(1.0) * step;
    highlight_column(&painter, plot, 7, hovered);
    y_axis(&painter, plot, max, step, &|v| compact(v));
    let slot = plot.width() / 7.0;
    let w = (slot * 0.56).min(36.0);
    for (i, parts) in weekdays.iter().enumerate() {
        let x = plot.left() + i as f32 * slot + (slot - w) / 2.0;
        stacked_bar(
            &painter,
            x,
            w,
            plot.bottom() - 1.0,
            plot.height() / max,
            *parts,
        );
        painter.text(
            pos2(x + w / 2.0, plot.bottom() + 4.0),
            Align2::CENTER_TOP,
            WEEKDAYS[i],
            small(),
            BRAND_FG_WEAK,
        );
    }
    hovered
}

pub fn ranked_rows(ui: &mut egui::Ui, rows: &[(String, u64)], name_width: f32) {
    let max = rows.first().map_or(1, |r| r.1.max(1)) as f32;
    for (name, n) in rows {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.text(
            pos2(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            name,
            FontId::proportional(13.0),
            BRAND_FG,
        );
        let count = super::group(*n);
        let bar_left = rect.left() + name_width;
        let bar_right = rect.right() - 64.0;
        let w = (bar_right - bar_left) * *n as f32 / max;
        painter.rect_filled(
            Rect::from_min_max(
                pos2(bar_left, rect.top() + 5.0),
                pos2(bar_left + w.max(2.0), rect.bottom() - 5.0),
            ),
            3.0,
            SERIES[0],
        );
        painter.text(
            pos2(rect.right(), rect.center().y),
            Align2::RIGHT_CENTER,
            count,
            FontId::proportional(13.0),
            BRAND_FG_WEAK,
        );
    }
}

pub fn hand_split(ui: &mut egui::Ui, left: u64, right: u64) {
    let total = (left + right).max(1) as f32;
    let share = left as f32 / total;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("Left {:.0}%", share * 100.0)).color(BRAND_COTTONTAIL),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("{:.0}% Right", (1.0 - share) * 100.0))
                    .color(BRAND_COTTONTAIL),
            );
        });
    });
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 12.0), Sense::hover());
    let split = rect.left() + rect.width() * share;
    let painter = ui.painter();
    painter.rect_filled(
        Rect::from_min_max(rect.min, pos2(split - SEGMENT_GAP / 2.0, rect.bottom())),
        CornerRadius {
            nw: 4,
            sw: 4,
            ne: 0,
            se: 0,
        },
        SERIES[0],
    );
    painter.rect_filled(
        Rect::from_min_max(pos2(split + SEGMENT_GAP / 2.0, rect.top()), rect.max),
        CornerRadius {
            nw: 0,
            sw: 0,
            ne: 4,
            se: 4,
        },
        BRAND_FG_WEAK,
    );
}

fn hour_label(h: usize) -> String {
    format!("{:02}", (h as u32 + DAY_START) % 24)
}

/// Hour ticks every `every` hours under a 24-column strip starting at `left`.
fn hour_ticks(painter: &egui::Painter, left: f32, slot: f32, y: f32, every: usize) {
    for h in (0..24).step_by(every) {
        painter.text(
            pos2(left + h as f32 * slot, y),
            Align2::CENTER_TOP,
            hour_label(h % 24),
            small(),
            BRAND_FG_WEAK,
        );
    }
}

/// 24 hour cells from `DAY_START`, tinted by the share of active days with input in that hour,
/// under a line from the usual start to the usual end.
pub fn typical_strip(
    ui: &mut egui::Ui,
    share: &[f32; 24],
    usual: Option<(u8, u8)>,
) -> Option<usize> {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 60.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let left = rect.left() + 8.0;
    let slot = (rect.width() - 16.0) / 24.0;
    let band_y = rect.top() + 5.0;
    let top = rect.top() + 13.0;
    let cells = Rect::from_min_max(pos2(left, top), pos2(left + 24.0 * slot, top + 28.0));
    let hovered = response
        .hover_pos()
        .filter(|p| cells.x_range().contains(p.x))
        .map(|p| (((p.x - left) / slot) as usize).min(23));
    for (h, v) in share.iter().enumerate() {
        let r = Rect::from_min_size(pos2(left + h as f32 * slot, top), vec2(slot - 2.0, 28.0));
        let fill = if *v > 0.0 {
            heat((v * 1000.0).max(1.0) as u64, 1000)
        } else {
            BRAND_DARKER
        };
        painter.rect_filled(r, 3.0, fill);
        if hovered == Some(h) {
            painter.rect_stroke(
                r,
                3.0,
                Stroke::new(1.5, BRAND_COTTONTAIL),
                egui::StrokeKind::Outside,
            );
        }
    }
    if let Some((first, end)) = usual {
        let x0 = left + f32::from(first) * slot;
        let x1 = left + f32::from(end) * slot - 2.0;
        let stroke = Stroke::new(2.0, BRAND_COTTONTAIL);
        painter.line_segment([pos2(x0, band_y), pos2(x1, band_y)], stroke);
        for x in [x0, x1] {
            painter.line_segment([pos2(x, band_y - 4.0), pos2(x, band_y + 4.0)], stroke);
        }
    }
    hour_ticks(&painter, left, slot, cells.bottom() + 4.0, 3);
    hovered
}

/// One row per day, oldest at the top, one cell per hour from `DAY_START`.
pub fn recent_grid(ui: &mut egui::Ui, rows: &[[u64; 24]], start: Date) -> Option<(usize, usize)> {
    const LABEL: f32 = 52.0;
    const ROW: f32 = 14.0;
    let height = rows.len() as f32 * ROW + AXIS_HEIGHT;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter_at(rect);
    let left = rect.left() + LABEL;
    let slot = (rect.right() - left) / 24.0;
    let max = rows.iter().flatten().copied().max().unwrap_or(0);
    let hovered = response.hover_pos().and_then(|p| {
        let (d, h) = ((p.y - rect.top()) / ROW, (p.x - left) / slot);
        (d >= 0.0 && h >= 0.0 && (d as usize) < rows.len() && (h as usize) < 24)
            .then_some((d as usize, h as usize))
    });
    for (d, row) in rows.iter().enumerate() {
        let y = rect.top() + d as f32 * ROW;
        let date = start.plus_days(d as i64);
        painter.text(
            pos2(rect.left(), y + ROW / 2.0),
            Align2::LEFT_CENTER,
            format!("{} {}", WEEKDAYS[date.weekday()], date.day),
            small(),
            BRAND_FG_WEAK,
        );
        let on: Vec<usize> = (0..24).filter(|h| row[*h] > 0).collect();
        if let (Some(first), Some(last)) = (on.first(), on.last()) {
            let track = Rect::from_min_max(
                pos2(left + *first as f32 * slot, y + 2.0),
                pos2(left + (*last + 1) as f32 * slot - 1.0, y + ROW - 2.0),
            );
            painter.rect_filled(track, 2.0, BRAND_BG_LIGHTEST);
        }
        for h in on {
            let r = Rect::from_min_size(
                pos2(left + h as f32 * slot, y + 2.0),
                vec2(slot - 1.0, ROW - 4.0),
            );
            painter.rect_filled(r, 2.0, heat(row[h], max));
            if hovered == Some((d, h)) {
                painter.rect_stroke(
                    r,
                    2.0,
                    Stroke::new(1.5, BRAND_COTTONTAIL),
                    egui::StrokeKind::Outside,
                );
            }
        }
    }
    hour_ticks(
        &painter,
        left,
        slot,
        rect.top() + rows.len() as f32 * ROW + 3.0,
        6,
    );
    hovered
}
