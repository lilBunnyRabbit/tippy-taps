use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Rect, RichText, Sense, Stroke, Vec2,
    ViewportCommand, pos2,
};

use crate::clock::{self, Date};
use crate::counts::{self, Counts, group};
use crate::{autostart, hooks, storage, tray};

const BRAND_DARKER: Color32 = Color32::from_rgb(0x0b, 0x15, 0x19);
const BRAND_BG: Color32 = Color32::from_rgb(0x17, 0x25, 0x2c);
const BRAND_BG_LIGHT: Color32 = Color32::from_rgb(0x1d, 0x2e, 0x36);
const BRAND_BG_LIGHTEST: Color32 = Color32::from_rgb(0x2a, 0x40, 0x4a);
const BRAND_FG: Color32 = Color32::from_rgb(0xb1, 0xab, 0xa8);
const BRAND_FG_WEAK: Color32 = Color32::from_rgb(0x7d, 0x80, 0x7f);
const BRAND_PRIMARY: Color32 = Color32::from_rgb(0xa4, 0xb4, 0x83);
const BRAND_PRIMARY_TINT: Color32 = Color32::from_rgb(0x2a, 0x37, 0x31);
const BRAND_COTTONTAIL: Color32 = Color32::from_rgb(0xe3, 0xf9, 0xb2);
const BRAND_WARNING: Color32 = Color32::from_rgb(0xe4, 0x9b, 0x39);
const BRAND_ERROR: Color32 = Color32::from_rgb(0xf8, 0x88, 0x76);
const HISTORY_DAYS: i64 = 30;
const REPAINT: Duration = Duration::from_millis(500);
const DATE_WIDTH: f32 = 84.0;
const TOTAL_WIDTH: f32 = 72.0;
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

static SHOWN: AtomicBool = AtomicBool::new(false);

pub fn show(ctx: &egui::Context) {
    SHOWN.store(true, Ordering::Relaxed);
    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(ViewportCommand::Focus);
    ctx.request_repaint();
}

fn hide(ctx: &egui::Context) {
    SHOWN.store(false, Ordering::Relaxed);
    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
}

struct Snapshot {
    today: Counts,
    total: Counts,
    history: Vec<(Date, Counts)>,
    path: String,
    error: Option<String>,
    notice: Option<String>,
}

impl Snapshot {
    fn take() -> Self {
        let today = clock::today();
        let storage = storage::get();
        let pending = counts::pending();
        let mut today_counts = storage.day(today);
        today_counts += pending;
        let mut total = storage.total();
        total += pending;
        let history = (0..HISTORY_DAYS)
            .map(|back| {
                let date = today.minus_days(back);
                let c = if back == 0 {
                    today_counts
                } else {
                    storage.day(date)
                };
                (date, c)
            })
            .collect();
        Self {
            today: today_counts,
            total,
            history,
            path: storage.path.display().to_string(),
            error: storage.error.clone(),
            notice: storage.notice.clone(),
        }
    }
}

pub struct App;

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        hooks::drop_raw_input();
        SHOWN.store(true, Ordering::Relaxed);
        tray::attach(cc.egui_ctx.clone());
        Self
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BRAND_BG.to_normalized_gamma_f32()
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            hide(ctx);
        }
        if SHOWN.load(Ordering::Relaxed) {
            ctx.request_repaint_after(REPAINT);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let snap = Snapshot::take();
        egui::Frame::new().inner_margin(16).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("Tippy Taps")
                    .size(20.0)
                    .color(BRAND_COTTONTAIL)
                    .strong(),
            );
            ui.add_space(12.0);

            caption(ui, "TODAY");
            ui.columns(3, |cols| {
                stat(&mut cols[0], snap.today.keys, "keys");
                stat(&mut cols[1], snap.today.clicks, "clicks");
                stat(&mut cols[2], snap.today.scrolls, "scrolls");
            });
            ui.add_space(12.0);

            caption(ui, "ALL TIME");
            ui.label(breakdown(snap.total));
            ui.add_space(12.0);

            caption(ui, "LAST 30 DAYS");
            let max = snap
                .history
                .iter()
                .map(|(_, c)| c.total())
                .max()
                .unwrap_or(0);
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.label(
                    RichText::new(format!(
                        "Counts only, never which keys. Stored locally at {}.",
                        snap.path
                    ))
                    .small()
                    .color(BRAND_FG_WEAK),
                );
                for text in [&snap.notice, &snap.error].into_iter().flatten() {
                    ui.colored_label(BRAND_WARNING, text);
                }
                if let Some(err) = hooks::error() {
                    ui.colored_label(BRAND_ERROR, err);
                }
                let mut on = autostart::enabled();
                if ui.checkbox(&mut on, "Start with Windows").changed() {
                    let _ = autostart::set(on);
                    tray::sync_autostart();
                }
                ui.add_space(4.0);
                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (date, c) in &snap.history {
                                day_row(ui, *date, *c, max);
                            }
                        });
                });
            });
        });
    }
}

fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().strong().color(BRAND_FG_WEAK));
    ui.add_space(2.0);
}

fn stat(ui: &mut egui::Ui, value: u64, label: &str) {
    let text = group(value);
    let size = if text.len() > 7 { 18.0 } else { 26.0 };
    egui::Frame::new()
        .fill(BRAND_BG_LIGHT)
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(8, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(text)
                        .size(size)
                        .color(BRAND_COTTONTAIL)
                        .strong(),
                );
                ui.label(RichText::new(label).color(BRAND_FG));
            });
        });
}

fn breakdown(c: Counts) -> String {
    format!(
        "{} keys · {} clicks · {} scrolls",
        group(c.keys),
        group(c.clicks),
        group(c.scrolls)
    )
}

fn day_label(date: Date) -> String {
    let weekday = WEEKDAYS[(date.to_days() + 4).rem_euclid(7) as usize];
    format!("{weekday} {} {}", date.day, MONTHS[date.month as usize - 1])
}

fn day_row(ui: &mut egui::Ui, date: Date, c: Counts, max: u64) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::hover());
    let painter = ui.painter();
    let mid = rect.top() + 9.0;
    let body = FontId::proportional(14.0);
    painter.text(
        pos2(rect.left(), mid),
        Align2::LEFT_CENTER,
        day_label(date),
        body.clone(),
        BRAND_FG,
    );
    painter.text(
        pos2(rect.right() - 4.0, mid),
        Align2::RIGHT_CENTER,
        group(c.total()),
        body,
        BRAND_COTTONTAIL,
    );
    let track = Rect::from_min_max(
        pos2(rect.left() + DATE_WIDTH, mid - 4.0),
        pos2(rect.right() - TOTAL_WIDTH, mid + 4.0),
    );
    painter.rect_filled(track, 4.0, BRAND_BG_LIGHTEST);
    if c.total() > 0 && max > 0 {
        let share = (c.total() as f32 / max as f32).max(0.02);
        let mut fill = track;
        fill.set_width(track.width() * share);
        painter.rect_filled(fill, 4.0, BRAND_PRIMARY);
    }
    painter.text(
        pos2(track.left(), mid + 16.0),
        Align2::LEFT_CENTER,
        breakdown(c),
        FontId::proportional(11.0),
        BRAND_FG_WEAK,
    );
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(BRAND_FG);
    v.panel_fill = BRAND_BG;
    v.window_fill = BRAND_BG;
    v.window_stroke = Stroke::new(1.0, BRAND_BG_LIGHTEST);
    v.extreme_bg_color = BRAND_DARKER;
    v.selection.bg_fill = BRAND_PRIMARY_TINT;
    v.selection.stroke = Stroke::new(1.0, BRAND_PRIMARY);
    v.hyperlink_color = BRAND_PRIMARY;
    v.warn_fg_color = BRAND_WARNING;
    v.error_fg_color = BRAND_ERROR;

    let w = &mut v.widgets;
    for state in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        state.corner_radius = CornerRadius::same(4);
        state.expansion = 0.0;
        state.bg_stroke = Stroke::NONE;
        state.fg_stroke = Stroke::new(1.0, BRAND_FG);
    }
    w.inactive.bg_fill = BRAND_BG_LIGHTEST;
    w.hovered.bg_fill = BRAND_BG_LIGHTEST;
    w.hovered.fg_stroke = Stroke::new(1.5, BRAND_COTTONTAIL);
    w.active.bg_fill = BRAND_BG_LIGHTEST;
    w.active.fg_stroke = Stroke::new(1.5, BRAND_COTTONTAIL);
    ctx.set_visuals(v);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_days_with_weekday() {
        let date = Date {
            year: 2026,
            month: 11,
            day: 20,
        };
        assert_eq!(day_label(date), "Fri 20 Nov");
    }
}
