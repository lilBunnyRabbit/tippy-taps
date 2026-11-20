use eframe::egui::{self, Align2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, vec2};

use super::charts::small;
use super::{
    BRAND_BG_LIGHTEST, BRAND_COTTONTAIL, BRAND_DARKER, BRAND_FG, BRAND_FG_WEAK, caption, card,
    group, heat, segmented, weak,
};
use crate::counts::{Bucket, move_distance};
use crate::mouse;

const SECTORS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
const MAP_HEIGHT: f32 = 260.0;
const FRAME_INSET: f32 = 3.0;
const SCREEN_PAD: f32 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer {
    Movement,
    Clicks,
}

pub struct Screens {
    list: Vec<mouse::Screen>,
    /// Per layer, the 99th percentile of non-zero cells: a few hot spots (taskbar, close
    /// buttons) would wash out everything else at the true maximum.
    max: [u64; 2],
}

impl Screens {
    pub fn new(list: Vec<mouse::Screen>) -> Self {
        let max = [Layer::Movement, Layer::Clicks].map(|layer| {
            let mut all: Vec<u64> = list
                .iter()
                .flat_map(|s| cells(&s.monitor, layer).iter().copied())
                .filter(|n| *n > 0)
                .collect();
            all.sort_unstable();
            all.get(all.len().saturating_sub(1) * 99 / 100)
                .copied()
                .unwrap_or(0)
        });
        Self { list, max }
    }
}

fn cells(m: &mouse::Monitor, layer: Layer) -> &[u64] {
    match layer {
        Layer::Movement => &m.movement,
        Layer::Clicks => &m.clicks,
    }
}

fn name(s: &mouse::Screen, i: usize) -> String {
    s.monitor
        .name
        .clone()
        .unwrap_or_else(|| format!("Display {}", i + 1))
}

pub struct MouseTab {
    layer: Layer,
    pinned: Option<(usize, usize)>,
}

impl MouseTab {
    pub fn new() -> Self {
        Self {
            layer: Layer::Movement,
            pinned: None,
        }
    }

    pub fn distance(ui: &mut egui::Ui, current: Bucket, previous: Option<(Bucket, &str)>) {
        caption(ui, "DISTANCE");
        ui.label(
            RichText::new(move_distance(current.m_um))
                .size(26.0)
                .color(BRAND_COTTONTAIL),
        );
        ui.label("moved on screen");
        weak(ui, format!("{} px", group(current.m_px)));
        if let Some((b, label)) = previous {
            ui.add_space(10.0);
            weak(ui, format!("{label} {}", move_distance(b.m_um)));
        }
    }

    pub fn direction(ui: &mut egui::Ui, b: Bucket) {
        caption(ui, "DIRECTION");
        let dirs = b.directions();
        let total = dirs.iter().sum::<u64>();
        let hovered = rose(ui, 150.0, &dirs);
        let text = match (hovered, total) {
            (_, 0) => "No movement yet".to_string(),
            (Some(i), _) => format!(
                "{} · {:.0}% of movement",
                SECTORS[i],
                dirs[i] as f64 * 100.0 / total as f64
            ),
            (None, _) => {
                let sideways = (dirs[2] + dirs[6]) as f64 / total as f64;
                format!("{:.0}% straight sideways", sideways * 100.0)
            }
        };
        weak(ui, text);
    }

    pub fn screens(&mut self, ui: &mut egui::Ui, screens: &Screens) {
        card(ui, |ui| {
            ui.horizontal(|ui| {
                caption(ui, "WHERE ON SCREEN · ALL TIME");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    segmented(
                        ui,
                        &mut self.layer,
                        &[(Layer::Movement, "Movement"), (Layer::Clicks, "Clicks")],
                    );
                });
            });
            ui.add_space(6.0);
            if !screens.list.iter().any(|s| s.connected) {
                weak(
                    ui,
                    "No cursor data yet. It fills in as you move the mouse.".into(),
                );
            } else {
                let hovered = self.map(ui, screens);
                ui.add_space(6.0);
                let text = hovered.or(self.pinned).map_or_else(
                    || "Hover or click a spot for its counts".to_string(),
                    |(s, c)| readout(screens, s, c),
                );
                weak(ui, text);
            }
            for (i, s) in screens
                .list
                .iter()
                .enumerate()
                .filter(|(_, s)| !s.connected)
            {
                let r = s.monitor.rect;
                weak(
                    ui,
                    format!(
                        "Not connected: {} · {}×{} · {} clicks",
                        name(s, i),
                        r.width(),
                        r.height(),
                        group(s.monitor.clicks.iter().sum())
                    ),
                );
            }
            weak(
                ui,
                "Counts per screen area only, never a path or a timeline.".into(),
            );
        });
    }

    fn map(&mut self, ui: &mut egui::Ui, screens: &Screens) -> Option<(usize, usize)> {
        let shown: Vec<(usize, &mouse::Screen)> = screens
            .list
            .iter()
            .enumerate()
            .filter(|(_, s)| s.connected)
            .collect();
        let (mut l, mut t, mut r, mut b) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for (_, s) in &shown {
            let m = s.monitor.rect;
            (l, t, r, b) = (l.min(m.left), t.min(m.top), r.max(m.right), b.max(m.bottom));
        }
        let width = ui.available_width();
        let scale = (width / (r - l).max(1) as f32).min(MAP_HEIGHT / (b - t).max(1) as f32);
        let size = vec2((r - l) as f32 * scale, (b - t) as f32 * scale);
        let (area, response) = ui.allocate_exact_size(vec2(width, size.y), Sense::click());
        let origin = area.min + vec2((width - size.x) / 2.0, 0.0);
        let at = |x: i32, y: i32| origin + vec2((x - l) as f32 * scale, (y - t) as f32 * scale);
        let layer_max = screens.max[self.layer as usize];
        let painter = ui.painter_at(area);
        let mut hovered = None;
        for (i, s) in shown {
            let m = &s.monitor;
            let frame =
                Rect::from_min_max(at(m.rect.left, m.rect.top), at(m.rect.right, m.rect.bottom))
                    .shrink(FRAME_INSET);
            painter.rect_filled(frame, 6.0, BRAND_DARKER);
            let inner = frame.shrink(SCREEN_PAD);
            let (cols, rows) = (m.cols as usize, m.rows as usize);
            let cell = vec2(inner.width() / cols as f32, inner.height() / rows as f32);
            if let Some(p) = response.hover_pos().filter(|p| inner.contains(*p)) {
                let c = (((p.x - inner.left()) / cell.x) as usize).min(cols - 1);
                let r = (((p.y - inner.top()) / cell.y) as usize).min(rows - 1);
                hovered = Some((i, r * cols + c));
            }
            dots(&painter, inner, cols, cells(m, self.layer), layer_max);
            let mark = hovered
                .filter(|h| h.0 == i)
                .or(self.pinned.filter(|p| p.0 == i));
            if let Some((_, c)) = mark {
                let r = Rect::from_min_size(
                    inner.min + vec2((c % cols) as f32 * cell.x, (c / cols) as f32 * cell.y),
                    cell,
                );
                painter.rect_stroke(
                    r,
                    2.0,
                    Stroke::new(1.5, BRAND_COTTONTAIL),
                    StrokeKind::Outside,
                );
            }
            painter.rect_stroke(
                frame,
                6.0,
                Stroke::new(1.0, BRAND_BG_LIGHTEST),
                StrokeKind::Inside,
            );
            let label = format!("{} · {}×{}", name(s, i), m.rect.width(), m.rect.height());
            let galley = painter.layout_no_wrap(label, small(), BRAND_FG);
            let pos = inner.min + vec2(6.0, 5.0);
            painter.rect_filled(
                Rect::from_min_size(pos - vec2(4.0, 2.0), galley.size() + vec2(8.0, 4.0)),
                4.0,
                BRAND_DARKER,
            );
            painter.galley(pos, galley, BRAND_FG);
        }
        if response.clicked() {
            self.pinned = if self.pinned == hovered {
                None
            } else {
                hovered
            };
        }
        hovered
    }
}

fn readout(screens: &Screens, s: usize, c: usize) -> String {
    let Some(screen) = screens.list.get(s) else {
        return String::new();
    };
    let m = &screen.monitor;
    let (cols, rows) = (m.cols as usize, m.rows as usize);
    let (w, h) = (
        m.rect.width().max(0) as usize,
        m.rect.height().max(0) as usize,
    );
    format!(
        "{} · around {}, {} · {} px moved · {} clicks",
        name(screen, s),
        ((c % cols) * 2 + 1) * w / (cols * 2),
        ((c / cols) * 2 + 1) * h / (rows * 2),
        group(m.movement.get(c).copied().unwrap_or(0)),
        group(m.clicks.get(c).copied().unwrap_or(0))
    )
}

fn dots(painter: &egui::Painter, inner: Rect, cols: usize, values: &[u64], max: u64) {
    let rows = values.len() / cols.max(1);
    let cell = vec2(
        inner.width() / cols as f32,
        inner.height() / rows.max(1) as f32,
    );
    let r_max = cell.x.min(cell.y) * 0.5;
    for (i, n) in values.iter().enumerate() {
        let c = inner.min
            + vec2(
                ((i % cols) as f32 + 0.5) * cell.x,
                ((i / cols) as f32 + 0.5) * cell.y,
            );
        if *n == 0 {
            painter.circle_filled(c, 0.8, BRAND_BG_LIGHTEST);
            continue;
        }
        let n = (*n).min(max);
        let t = (n as f32 / max.max(1) as f32).sqrt();
        painter.circle_filled(c, (r_max * t).max(1.2), heat(n, max));
    }
}

/// Eight wedges, radius by the square root of the distance so wedge area tracks it.
fn rose(ui: &mut egui::Ui, size: f32, dirs: &[u64; 8]) -> Option<usize> {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), size), Sense::hover());
    let painter = ui.painter_at(rect);
    let c = rect.center();
    let r_max = size / 2.0 - 14.0;
    let max = dirs.iter().copied().max().unwrap_or(0);
    let hovered = response.hover_pos().and_then(|p| {
        let d = p - c;
        (max > 0 && d.length() <= r_max + 4.0 && d.length() > 2.0).then(|| {
            let a = d.x.atan2(-d.y).to_degrees().rem_euclid(360.0);
            (((a + 22.5) / 45.0) as usize) % 8
        })
    });
    for ring in [0.5, 1.0] {
        painter.circle_stroke(c, r_max * ring, Stroke::new(1.0, BRAND_BG_LIGHTEST));
    }
    for (i, n) in dirs.iter().enumerate().filter(|(_, n)| **n > 0) {
        let len = r_max * (*n as f32 / max as f32).sqrt();
        let mid = (i as f32 * 45.0).to_radians();
        let half = 20.0f32.to_radians();
        let mut pts = vec![c];
        for k in 0..=8 {
            let a = mid - half + 2.0 * half * k as f32 / 8.0;
            pts.push(c + Vec2::angled(a - std::f32::consts::FRAC_PI_2) * len);
        }
        let mut color = heat(*n, max);
        if hovered.is_some_and(|h| h != i) {
            color = color.gamma_multiply(0.6);
        }
        painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    }
    for (i, label) in ["N", "E", "S", "W"].iter().enumerate() {
        let a = (i as f32 * 90.0).to_radians();
        painter.text(
            c + Vec2::angled(a - std::f32::consts::FRAC_PI_2) * (r_max + 8.0),
            Align2::CENTER_CENTER,
            *label,
            small(),
            BRAND_FG_WEAK,
        );
    }
    hovered
}
