use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::Thread;
use std::time::Duration;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Id, LayerId, Order, Rect, RichText, Sense, Stroke,
    Vec2, ViewportCommand, pos2, vec2,
};

use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};

use crate::buttons::Buttons;
use crate::clock::{self, Date, Hour};
use crate::counts::{self, Bucket, duration, group, scroll_distance};
use crate::fireworks::Fireworks;
use crate::stats::{self, CALENDAR_WEEKS, Period, Stats};
use crate::{autostart, hooks, keyboard, milestones, settings, storage, tray};

pub(crate) const BRAND_DARKER: Color32 = Color32::from_rgb(0x0b, 0x15, 0x19);
const BRAND_BG: Color32 = Color32::from_rgb(0x17, 0x25, 0x2c);
pub(crate) const BRAND_BG_LIGHT: Color32 = Color32::from_rgb(0x1d, 0x2e, 0x36);
pub(crate) const BRAND_BG_LIGHTEST: Color32 = Color32::from_rgb(0x2a, 0x40, 0x4a);
pub(crate) const BRAND_FG: Color32 = Color32::from_rgb(0xb1, 0xab, 0xa8);
pub(crate) const BRAND_FG_WEAK: Color32 = Color32::from_rgb(0x7d, 0x80, 0x7f);
const BRAND_PRIMARY: Color32 = Color32::from_rgb(0xa4, 0xb4, 0x83);
const BRAND_PRIMARY_TINT: Color32 = Color32::from_rgb(0x2a, 0x37, 0x31);
pub(crate) const BRAND_COTTONTAIL: Color32 = Color32::from_rgb(0xe3, 0xf9, 0xb2);
const BRAND_WARNING: Color32 = Color32::from_rgb(0xe4, 0x9b, 0x39);
const BRAND_ERROR: Color32 = Color32::from_rgb(0xf8, 0x88, 0x76);
const CHANGE_CHECK: Duration = Duration::from_millis(500);
const LABEL_WIDTH: f32 = 30.0;
const LABEL_HEIGHT: f32 = 14.0;
const GAP: f32 = 2.0;
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

static SHOWN: AtomicBool = AtomicBool::new(false);
static WATCHER: OnceLock<Thread> = OnceLock::new();

pub fn show(ctx: &egui::Context) {
    SHOWN.store(true, Ordering::Relaxed);
    if let Some(thread) = WATCHER.get() {
        thread.unpark();
    }
    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(ViewportCommand::Focus);
    ctx.request_repaint();
}

fn hide(ctx: &egui::Context) {
    SHOWN.store(false, Ordering::Relaxed);
    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
}

pub fn shown() -> bool {
    SHOWN.load(Ordering::Relaxed)
}

pub(crate) type Fingerprint = (u64, Hour, Bucket, u64);

fn fingerprint() -> Fingerprint {
    (
        storage::revision(),
        clock::now_hour(),
        counts::pending(),
        keyboard::revision(),
    )
}

/// Wakes egui only when something on screen changed, and parks while the window is hidden, so
/// an open window costs nothing between taps and a hidden one costs nothing at all.
fn spawn_watcher(ctx: egui::Context) {
    let handle = std::thread::Builder::new()
        .name("repaint".into())
        .spawn(move || {
            let mut last = fingerprint();
            let mut trimmed = false;
            loop {
                if !shown() {
                    if !trimmed {
                        trim_working_set();
                        trimmed = true;
                    }
                    std::thread::park();
                    continue;
                }
                trimmed = false;
                std::thread::sleep(CHANGE_CHECK);
                let now = fingerprint();
                if now != last {
                    last = now;
                    ctx.request_repaint();
                }
            }
        })
        .expect("spawn repaint thread");
    let _ = WATCHER.set(handle.thread().clone());
}

/// eframe cannot destroy its root window, so a hidden window keeps its GL context and buffers.
/// Handing those pages back to Windows on hide keeps the tray-only footprint small; they page
/// back in when the window opens again. Runs here, after the hide has been applied.
fn trim_working_set() {
    unsafe {
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

struct View {
    key: Fingerprint,
    stats: Stats,
    keys: Box<keyboard::Counts>,
}

pub struct App {
    period: Period,
    revealed: bool,
    view: Option<View>,
    buttons: Buttons,
    pinned_hour: Option<(usize, usize)>,
    pinned_day: Option<(usize, usize)>,
    fireworks: Fireworks,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);
        hooks::drop_raw_input();
        SHOWN.store(true, Ordering::Relaxed);
        spawn_watcher(cc.egui_ctx.clone());
        tray::attach(cc.egui_ctx.clone());
        let seed = clock::LocalTime::now().ms_of_day as u64 ^ 0x9e37_79b9_7f4a_7c15;
        Self {
            period: Period::Day,
            revealed: false,
            view: None,
            buttons: Buttons::new(),
            pinned_hour: None,
            pinned_day: None,
            fireworks: Fireworks::new(
                [BRAND_COTTONTAIL, BRAND_PRIMARY, BRAND_WARNING, BRAND_ERROR],
                seed,
            ),
        }
    }

    /// Recomputed only when the fingerprint moves, not every frame.
    fn take_view(&mut self) -> View {
        let key = fingerprint();
        match self.view.take() {
            Some(view) if view.key == key => view,
            _ => {
                let stats = {
                    let storage = storage::get();
                    stats::compute(&storage.hours, storage.total, key.2, key.1)
                };
                let keys = if keyboard::enabled() {
                    keyboard::counts()
                } else {
                    Box::new([0; keyboard::SLOTS])
                };
                View { key, stats, keys }
            }
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        BRAND_BG.to_normalized_gamma_f32()
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.revealed = false;
            hide(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let view = self.take_view();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new().inner_margin(16).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    self.content(ui, &view.stats);
                    ui.add_space(12.0);
                    self.buttons.show(ui, &view.stats, &view.keys, view.key);
                    ui.add_space(12.0);
                    self.footer(ui);
                });
            });
        self.view = Some(view);
        self.play_fireworks(ui.ctx());
    }
}

impl App {
    /// Plays only while the window is on screen; a show cut short by closing or minimising
    /// pauses and resumes when the window is back.
    fn play_fireworks(&mut self, ctx: &egui::Context) {
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        if !shown() || minimized {
            return;
        }
        for m in milestones::take_queued() {
            self.fireworks.launch(m.label());
        }
        if self.fireworks.active() {
            let area = ctx.content_rect();
            self.fireworks.step(ctx.input(|i| i.stable_dt), area);
            let painter =
                ctx.layer_painter(LayerId::new(Order::Foreground, Id::unique("fireworks")));
            self.fireworks.paint(&painter, area, BRAND_DARKER);
            ctx.request_repaint();
        }
    }

    fn content(&mut self, ui: &mut egui::Ui, stats: &Stats) {
        ui.label(
            RichText::new("Tippy Taps")
                .size(20.0)
                .color(BRAND_COTTONTAIL)
                .strong(),
        );
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            for p in Period::ALL {
                ui.selectable_value(&mut self.period, p, p.label());
            }
        });
        ui.add_space(8.0);
        let (current, previous) = stats.period(self.period);
        caption(ui, self.period.current_label());
        ui.columns(3, |cols| {
            stat(&mut cols[0], current.keys, "keys");
            stat(&mut cols[1], current.clicks, "clicks");
            stat(&mut cols[2], current.scrolls, "scrolls");
        });
        ui.add_space(4.0);
        weak(ui, format!("{} scrolled", scroll_distance(current.scrolls)));
        weak(
            ui,
            format!(
                "{}: {} · {} scrolled",
                self.period.previous_label(),
                breakdown(previous),
                scroll_distance(previous.scrolls)
            ),
        );
        ui.add_space(12.0);

        if stats.total.has_controller() {
            caption(ui, "CONTROLLER");
            controller(ui, current);
            ui.add_space(12.0);
        }

        caption(ui, "ALL TIME");
        ui.label(breakdown(stats.total));
        weak(
            ui,
            format!("{} scrolled", scroll_distance(stats.total.scrolls)),
        );
        ui.horizontal(|ui| {
            ui.label("Gross total:");
            if self.revealed {
                ui.label(
                    RichText::new(group(stats.total.total()))
                        .color(BRAND_COTTONTAIL)
                        .strong(),
                );
            } else if ui.button("tap to reveal").clicked() {
                self.revealed = true;
            }
        });
        ui.add_space(12.0);

        caption(ui, "TIME OF DAY");
        let cell = |(col, row): (usize, usize)| stats.time_of_day[row][col];
        let max = stats.time_of_day.iter().flatten().map(Bucket::total).max();
        let hour_labels = [(0, "0"), (6, "6"), (12, "12"), (18, "18")]
            .map(|(c, t)| (c, t.to_string()))
            .to_vec();
        let hovered = heatmap(
            ui,
            (24, 7),
            &hour_labels,
            &|_| true,
            &|pos| cell(pos).total(),
            max.unwrap_or(0),
            &mut self.pinned_hour,
        );
        detail(
            ui,
            hovered.or(self.pinned_hour).map(|(h, d)| {
                format!(
                    "{} {h:02}:00–{:02}:00 · {}",
                    WEEKDAYS[d],
                    h + 1,
                    totals(cell((h, d)))
                )
            }),
        );
        ui.add_space(12.0);

        caption(ui, "LAST 53 WEEKS");
        let today = (stats.now.date.to_days() - stats.calendar_start.to_days()) as usize;
        let day = |(week, weekday): (usize, usize)| week * 7 + weekday;
        let max = stats.days.iter().map(Bucket::total).max().unwrap_or(0);
        let hovered = heatmap(
            ui,
            (CALENDAR_WEEKS, 7),
            &month_labels(stats.calendar_start),
            &|pos| day(pos) <= today,
            &|pos| stats.days[day(pos)].total(),
            max,
            &mut self.pinned_day,
        );
        detail(
            ui,
            hovered.or(self.pinned_day).map(|pos| {
                let date = stats.calendar_start.plus_days(day(pos) as i64);
                format!("{} · {}", day_label(date), totals(stats.days[day(pos)]))
            }),
        );
    }

    fn footer(&self, ui: &mut egui::Ui) {
        let mut on = autostart::enabled();
        if ui.checkbox(&mut on, "Start with Windows").changed() {
            let _ = autostart::set(on);
            tray::sync_menu();
        }
        let mut fireworks = settings::fireworks();
        if ui
            .checkbox(&mut fireworks, "Fireworks at milestones")
            .changed()
        {
            settings::update(|s| s.fireworks = fireworks);
            tray::sync_menu();
        }
        ui.add_space(8.0);

        let (dir, notice, error) = {
            let storage = storage::get();
            (
                storage.dir.display().to_string(),
                storage.notice.clone(),
                storage.error.clone(),
            )
        };
        for text in [notice, error, settings::error()]
            .into_iter()
            .flatten()
            .chain(keyboard::messages())
        {
            ui.colored_label(BRAND_WARNING, text);
        }
        if let Some(err) = hooks::error() {
            ui.colored_label(BRAND_ERROR, err);
        }
        let privacy = if keyboard::enabled() {
            "Counts only, plus presses per key position. Never what you typed, the order, or when."
        } else {
            "Counts only, never which keys."
        };
        weak(ui, format!("{privacy} Stored locally at {dir}."));
    }
}

pub(crate) fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().strong().color(BRAND_FG_WEAK));
    ui.add_space(2.0);
}

pub(crate) fn weak(ui: &mut egui::Ui, text: String) {
    ui.label(RichText::new(text).small().color(BRAND_FG_WEAK));
}

fn detail(ui: &mut egui::Ui, text: Option<String>) {
    weak(
        ui,
        text.unwrap_or_else(|| "Hover or click a cell for its counts".into()),
    );
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

fn controller(ui: &mut egui::Ui, c: Bucket) {
    egui::Grid::new("controller")
        .num_columns(3)
        .spacing(vec2(16.0, 4.0))
        .show(ui, |ui| {
            ui.label("Buttons");
            ui.label(RichText::new(group(c.buttons)).color(BRAND_COTTONTAIL));
            ui.label("");
            ui.end_row();
            for (name, pulls, ms) in [("LT", c.lt_pulls, c.lt_ms), ("RT", c.rt_pulls, c.rt_ms)] {
                ui.label(name);
                ui.label(RichText::new(format!("{} pulls", group(pulls))).color(BRAND_COTTONTAIL));
                ui.label(format!("held {}", duration(ms)));
                ui.end_row();
            }
        });
}

fn breakdown(c: Bucket) -> String {
    let mut text = format!(
        "{} keys · {} clicks · {} scrolls",
        group(c.keys),
        group(c.clicks),
        group(c.scrolls)
    );
    if c.has_controller() {
        text.push_str(&format!(
            " · {} buttons · LT {} · RT {}",
            group(c.buttons),
            group(c.lt_pulls),
            group(c.rt_pulls)
        ));
    }
    text
}

fn totals(c: Bucket) -> String {
    format!("{} inputs: {}", group(c.total()), breakdown(c))
}

fn day_label(date: Date) -> String {
    format!(
        "{} {} {} {}",
        WEEKDAYS[date.weekday()],
        date.day,
        MONTHS[date.month as usize - 1],
        date.year
    )
}

fn month_labels(start: Date) -> Vec<(usize, String)> {
    (0..CALENDAR_WEEKS)
        .filter_map(|week| {
            let monday = start.plus_days(week as i64 * 7);
            let sunday = monday.plus_days(6);
            (week > 0 && sunday.day <= 7)
                .then(|| (week, MONTHS[sunday.month as usize - 1].to_string()))
        })
        .collect()
}

pub(crate) fn heat(value: u64, max: u64) -> Color32 {
    if value == 0 || max == 0 {
        return BRAND_BG_LIGHT;
    }
    let t = (value as f32 / max as f32).sqrt();
    if t < 0.5 {
        BRAND_PRIMARY_TINT.lerp_to_gamma(BRAND_PRIMARY, 0.25 + t * 1.5)
    } else {
        BRAND_PRIMARY.lerp_to_gamma(BRAND_COTTONTAIL, (t - 0.5) * 2.0)
    }
}

/// Columns left to right, rows top to bottom (Monday first). Returns the hovered cell; a click
/// pins a cell, a second click on it unpins.
fn heatmap(
    ui: &mut egui::Ui,
    (cols, rows): (usize, usize),
    col_labels: &[(usize, String)],
    exists: &dyn Fn((usize, usize)) -> bool,
    value: &dyn Fn((usize, usize)) -> u64,
    max: u64,
    pinned: &mut Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    let width = ui.available_width();
    let cell_w = ((width - LABEL_WIDTH) / cols as f32 - GAP).max(1.0);
    let cell_h = cell_w.clamp(4.0, 14.0);
    let height = LABEL_HEIGHT + rows as f32 * (cell_h + GAP);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let origin = rect.min + vec2(LABEL_WIDTH, LABEL_HEIGHT);
    let cell_at = |p: egui::Pos2| {
        let rel = p - origin;
        let (col, row) = (
            (rel.x / (cell_w + GAP)).floor(),
            (rel.y / (cell_h + GAP)).floor(),
        );
        (col >= 0.0 && row >= 0.0 && (col as usize) < cols && (row as usize) < rows)
            .then_some((col as usize, row as usize))
            .filter(|pos| exists(*pos))
    };
    let hovered = response.hover_pos().and_then(cell_at);
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos().and_then(cell_at)
    {
        *pinned = if *pinned == Some(pos) {
            None
        } else {
            Some(pos)
        };
    }

    let painter = ui.painter_at(rect);
    let small = FontId::proportional(10.0);
    for (row, name) in WEEKDAYS.iter().enumerate().take(rows) {
        if rows < 7 || row % 2 == 0 {
            painter.text(
                pos2(
                    rect.left(),
                    origin.y + row as f32 * (cell_h + GAP) + cell_h / 2.0,
                ),
                Align2::LEFT_CENTER,
                *name,
                small.clone(),
                BRAND_FG_WEAK,
            );
        }
    }
    for (col, text) in col_labels {
        painter.text(
            pos2(origin.x + *col as f32 * (cell_w + GAP), rect.top()),
            Align2::LEFT_TOP,
            text,
            small.clone(),
            BRAND_FG_WEAK,
        );
    }
    for col in 0..cols {
        for row in 0..rows {
            if !exists((col, row)) {
                continue;
            }
            let min = origin + vec2(col as f32 * (cell_w + GAP), row as f32 * (cell_h + GAP));
            let cell = Rect::from_min_size(min, Vec2::new(cell_w, cell_h));
            painter.rect_filled(cell, 2.0, heat(value((col, row)), max));
            if *pinned == Some((col, row)) || hovered == Some((col, row)) {
                painter.rect_stroke(
                    cell.expand(1.0),
                    2.0,
                    Stroke::new(1.0, BRAND_COTTONTAIL),
                    egui::StrokeKind::Outside,
                );
            }
        }
    }
    hovered
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
        assert_eq!(day_label(date), "Fri 20 Nov 2026");
    }

    #[test]
    fn month_labels_mark_the_first_week_of_each_month() {
        let start = Date {
            year: 2025,
            month: 11,
            day: 17,
        };
        let labels = month_labels(start);
        assert_eq!(labels[0], (2, "Dec".to_string()));
        assert_eq!(labels.len(), 12);
    }

    #[test]
    fn breakdown_adds_controller_only_when_present() {
        let mut b = Bucket {
            keys: 1_234,
            ..Default::default()
        };
        assert_eq!(breakdown(b), "1,234 keys · 0 clicks · 0 scrolls");
        b.lt_pulls = 3;
        assert_eq!(
            breakdown(b),
            "1,234 keys · 0 clicks · 0 scrolls · 0 buttons · LT 3 · RT 0"
        );
        assert_eq!(
            totals(b),
            "1,237 inputs: 1,234 keys · 0 clicks · 0 scrolls · 0 buttons · LT 3 · RT 0"
        );
    }
}
