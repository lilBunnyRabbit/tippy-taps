use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread::Thread;
use std::time::Duration;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Id, LayerId, Layout, Order,
    Rect, RectAlign, RichText, Sense, Stroke, TextStyle, TextureHandle, TextureOptions, Vec2,
    ViewportCommand, pos2, vec2,
};

use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};

use crate::buttons::{Buttons, Device};
use crate::clock::{self, Date, Hour};
use crate::counts::{self, Bucket, duration, group, scroll_distance};
use crate::fireworks::Fireworks;
use crate::stats::{self, CALENDAR_WEEKS, Period, Stats};
use crate::{autostart, hooks, keyboard, milestones, overlay, settings, storage, tray};

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
/// How often a shown window looks for new taps to drum; data repaints stay at `CHANGE_CHECK`.
const DRUM_CHECK: Duration = Duration::from_millis(33);
const CHANGE_CHECK: u32 = 15;
const PAW_HOLD: f64 = 0.11;
const PAGE_MARGIN: i8 = 24;
const RABBIT_SIZE: f32 = 92.0;
const SIDE_CARD_HEIGHT: f32 = 132.0;
const LABEL_WIDTH: f32 = 34.0;
const LABEL_HEIGHT: f32 = 16.0;
const GAP: f32 = 3.0;
const LEFT: u8 = 1;
const RIGHT: u8 = 2;
const BOTH: u8 = LEFT | RIGHT;
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
/// Idle, left paw, right paw, both, cheer. "Left" is the paw on the viewer's left.
const RABBIT: [(&str, &[u8]); 5] = [
    ("idle", include_bytes!("../assets/rabbit/idle-256.png")),
    ("left", include_bytes!("../assets/rabbit/left-256.png")),
    ("right", include_bytes!("../assets/rabbit/right-256.png")),
    ("both", include_bytes!("../assets/rabbit/both-256.png")),
    ("cheer", include_bytes!("../assets/rabbit/cheer-256.png")),
];

static SHOWN: AtomicBool = AtomicBool::new(false);
static WATCHER: OnceLock<Thread> = OnceLock::new();
static SIDES: AtomicU8 = AtomicU8::new(0);

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

/// Which paws the taps between two pending snapshots drum. Mouse buttons keep their side, wheels
/// and middle/side buttons hit both, and single keys or pad buttons alternate like a drum roll.
fn drum_sides(before: Bucket, after: Bucket, alternate: &mut bool) -> u8 {
    let up = |a: u64, b: u64| b.saturating_sub(a);
    let mut sides = 0;
    if up(before.m_left, after.m_left) > 0 {
        sides |= LEFT;
    }
    if up(before.m_right, after.m_right) > 0 {
        sides |= RIGHT;
    }
    if up(before.m_middle, after.m_middle)
        + up(before.m_x1, after.m_x1)
        + up(before.m_x2, after.m_x2)
        + up(before.scrolls, after.scrolls)
        > 0
    {
        sides |= BOTH;
    }
    let taps = up(before.keys, after.keys)
        + up(before.buttons, after.buttons)
        + up(before.lt_pulls, after.lt_pulls)
        + up(before.rt_pulls, after.rt_pulls);
    match taps {
        0 => {}
        1 => {
            sides |= if *alternate { RIGHT } else { LEFT };
            *alternate = !*alternate;
        }
        _ => sides |= BOTH,
    }
    sides
}

/// Wakes egui only when the rabbit has a tap to drum or something on screen changed, and parks
/// while the window is hidden, so an open window repaints only on input and a hidden one costs
/// nothing at all.
fn spawn_watcher(ctx: egui::Context) {
    let handle = std::thread::Builder::new()
        .name("repaint".into())
        .spawn(move || {
            let mut last = fingerprint();
            let mut pending = last.2;
            let mut alternate = false;
            let mut tick = 0;
            let mut trimmed = false;
            loop {
                if !shown() {
                    if !trimmed {
                        trim_working_set();
                        trimmed = true;
                    }
                    std::thread::park();
                    pending = counts::pending();
                    continue;
                }
                trimmed = false;
                std::thread::sleep(DRUM_CHECK);
                let now = counts::pending();
                let sides = drum_sides(pending, now, &mut alternate);
                pending = now;
                if sides != 0 {
                    SIDES.fetch_or(sides, Ordering::Relaxed);
                    ctx.request_repaint();
                }
                tick = (tick + 1) % CHANGE_CHECK;
                if tick == 0 {
                    let now = fingerprint();
                    if now != last {
                        last = now;
                        ctx.request_repaint();
                    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Counts,
    When,
    Keyboard,
    Mouse,
    Controller,
}

impl Tab {
    const ALL: [(Self, &'static str); 5] = [
        (Self::Counts, "Counts"),
        (Self::When, "When"),
        (Self::Keyboard, "Keyboard"),
        (Self::Mouse, "Mouse"),
        (Self::Controller, "Controller"),
    ];
}

#[derive(Default)]
struct Paws {
    left_until: f64,
    right_until: f64,
}

impl Paws {
    fn hit(&mut self, sides: u8, now: f64) {
        if sides & LEFT != 0 {
            self.left_until = now + PAW_HOLD;
        }
        if sides & RIGHT != 0 {
            self.right_until = now + PAW_HOLD;
        }
    }

    fn frame(&self, now: f64, cheering: bool) -> usize {
        if cheering {
            return 4;
        }
        match (now < self.left_until, now < self.right_until) {
            (false, false) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (true, true) => 3,
        }
    }

    fn next_change(&self, now: f64) -> Option<f64> {
        [self.left_until, self.right_until]
            .into_iter()
            .filter(|t| *t > now)
            .reduce(f64::min)
    }
}

struct View {
    key: Fingerprint,
    stats: Stats,
    keys: Box<keyboard::Counts>,
    computed_at: f64,
}

pub struct App {
    tab: Tab,
    period: Period,
    revealed: bool,
    view: Option<View>,
    buttons: Buttons,
    pinned_hour: Option<(usize, usize)>,
    pinned_day: Option<(usize, usize)>,
    fireworks: Fireworks,
    rabbit: Vec<TextureHandle>,
    paws: Paws,
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
            tab: Tab::Counts,
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
            rabbit: RABBIT
                .iter()
                .map(|(name, png)| rabbit_texture(&cc.egui_ctx, name, png))
                .collect(),
            paws: Paws::default(),
        }
    }

    /// Recomputed when the fingerprint moves, at most every half second: drumming repaints on
    /// every tap, the aggregates need not follow that fast.
    fn take_view(&mut self, ctx: &egui::Context) -> View {
        let key = fingerprint();
        let now = ctx.input(|i| i.time);
        let wait = CHANGE_CHECK as f64 * DRUM_CHECK.as_secs_f64();
        match self.view.take() {
            Some(view) if view.key == key => view,
            Some(view) if now - view.computed_at < wait => {
                ctx.request_repaint_after(Duration::from_secs_f64(wait - (now - view.computed_at)));
                view
            }
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
                View {
                    key,
                    stats,
                    keys,
                    computed_at: now,
                }
            }
        }
    }
}

fn rabbit_texture(ctx: &egui::Context, name: &str, png: &[u8]) -> TextureHandle {
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .expect("bundled rabbit art")
        .to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    ctx.load_texture(
        format!("rabbit-{name}"),
        egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
        TextureOptions {
            mipmap_mode: Some(egui::TextureFilter::Linear),
            ..TextureOptions::LINEAR
        },
    )
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
        let ctx = ui.ctx().clone();
        let view = self.take_view(&ctx);
        self.queue_fireworks(&ctx);
        self.header(ui, &view.stats);
        egui::Panel::bottom("footer")
            .frame(
                egui::Frame::new()
                    .fill(BRAND_BG)
                    .inner_margin(egui::Margin {
                        left: PAGE_MARGIN,
                        right: PAGE_MARGIN,
                        top: 8,
                        bottom: 12,
                    }),
            )
            .show(ui, footer);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BRAND_BG))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(egui::Margin::symmetric(PAGE_MARGIN, 20))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                self.page(ui, &view);
                            });
                    });
            });
        self.view = Some(view);
        self.play_fireworks(&ctx);
    }
}

impl App {
    /// Fireworks wait for a window that is on screen; a show cut short by closing or minimising
    /// pauses and resumes when the window is back.
    fn on_screen(ctx: &egui::Context) -> bool {
        shown() && !ctx.input(|i| i.viewport().minimized.unwrap_or(false))
    }

    fn queue_fireworks(&mut self, ctx: &egui::Context) {
        if Self::on_screen(ctx) {
            for m in milestones::take_queued() {
                self.fireworks.launch(m.label());
            }
        }
    }

    fn play_fireworks(&mut self, ctx: &egui::Context) {
        if Self::on_screen(ctx) && self.fireworks.active() {
            let area = ctx.content_rect();
            self.fireworks.step(ctx.input(|i| i.stable_dt), area);
            let painter =
                ctx.layer_painter(LayerId::new(Order::Foreground, Id::unique("fireworks")));
            self.fireworks.paint(&painter, area, BRAND_DARKER);
            ctx.request_repaint();
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, stats: &Stats) {
        egui::Panel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(BRAND_DARKER)
                    .inner_margin(egui::Margin {
                        left: PAGE_MARGIN,
                        right: PAGE_MARGIN,
                        top: 14,
                        bottom: 0,
                    }),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    self.drum(ui);
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        ui.add_space(24.0);
                        ui.label(
                            RichText::new("Tippy Taps")
                                .size(24.0)
                                .color(BRAND_COTTONTAIL),
                        );
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let today = stats.period(Period::Day).0.total();
                            ui.label(RichText::new(group(today)).color(BRAND_COTTONTAIL));
                            ui.label("taps today");
                        });
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), settings_menu);
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (tab, label) in Tab::ALL {
                        if tab_button(ui, self.tab == tab, label).clicked() {
                            self.tab = tab;
                        }
                    }
                });
            });
    }

    /// The header rabbit: a paw down for each tap the watcher saw, the cheer frame while
    /// fireworks play. Repaints are booked only for the moment a paw goes back up.
    fn drum(&mut self, ui: &mut egui::Ui) {
        let now = ui.input(|i| i.time);
        self.paws.hit(SIDES.swap(0, Ordering::Relaxed), now);
        let frame = self.paws.frame(now, self.fireworks.active());
        if let Some(at) = self.paws.next_change(now) {
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(at - now));
        }
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(RABBIT_SIZE), Sense::hover());
        let painter = ui.painter();
        painter.circle_filled(rect.center(), RABBIT_SIZE / 2.0, BRAND_BG_LIGHTEST);
        painter.image(
            self.rabbit[frame].id(),
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    fn page(&mut self, ui: &mut egui::Ui, view: &View) {
        match self.tab {
            Tab::Counts => self.counts(ui, &view.stats),
            Tab::When => self.when(ui, &view.stats),
            Tab::Keyboard => {
                self.buttons
                    .show(ui, Device::Keyboard, &view.stats, &view.keys, view.key)
            }
            Tab::Mouse => self
                .buttons
                .show(ui, Device::Mouse, &view.stats, &view.keys, view.key),
            Tab::Controller => {
                self.buttons
                    .show(ui, Device::Controller, &view.stats, &view.keys, view.key)
            }
        }
    }

    fn counts(&mut self, ui: &mut egui::Ui, stats: &Stats) {
        let (current, previous) = stats.period(self.period);
        let before = self.period.previous_label();
        ui.horizontal(|ui| {
            caption(
                ui,
                &format!(
                    "{} · COMPARED WITH {}",
                    self.period.current_label(),
                    before.to_uppercase()
                ),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let options = Period::ALL.map(|p| (p, p.label()));
                segmented(ui, &mut self.period, &options);
            });
        });
        ui.add_space(12.0);
        ui.columns(3, |cols| {
            stat(
                &mut cols[0],
                current.keys,
                "keys",
                format!("{before} {}", group(previous.keys)),
            );
            stat(
                &mut cols[1],
                current.clicks,
                "clicks",
                format!("{before} {}", group(previous.clicks)),
            );
            stat(
                &mut cols[2],
                current.scrolls,
                &format!("scrolls · {}", scroll_distance(current.scrolls)),
                format!(
                    "{before} {} · {}",
                    group(previous.scrolls),
                    scroll_distance(previous.scrolls)
                ),
            );
        });
        ui.add_space(14.0);

        let controller = stats.total.has_controller();
        let all_time = |ui: &mut egui::Ui, revealed: &mut bool| {
            card(ui, |ui| {
                ui.set_min_height(SIDE_CARD_HEIGHT);
                caption(ui, "ALL TIME");
                all_time(ui, stats.total, revealed);
            });
        };
        if controller {
            ui.columns(2, |cols| {
                all_time(&mut cols[0], &mut self.revealed);
                card(&mut cols[1], |ui| {
                    ui.set_min_height(SIDE_CARD_HEIGHT);
                    caption(ui, "CONTROLLER");
                    controller_table(ui, current, previous, before);
                });
            });
        } else {
            all_time(ui, &mut self.revealed);
        }
    }

    fn when(&mut self, ui: &mut egui::Ui, stats: &Stats) {
        caption(ui, "LAST 53 WEEKS");
        let today = (stats.now.date.to_days() - stats.calendar_start.to_days()) as usize;
        let day = |(week, weekday): (usize, usize)| week * 7 + weekday;
        let max = stats.days.iter().map(Bucket::total).max().unwrap_or(0);
        let hovered = heatmap(
            ui,
            (CALENDAR_WEEKS, 7),
            16.0,
            &month_labels(stats.calendar_start),
            &|pos| day(pos) <= today,
            &|pos| stats.days[day(pos)].total(),
            max,
            &mut self.pinned_day,
        );
        let text = hovered.or(self.pinned_day).map_or_else(
            || "Hover or click a day for its counts".to_string(),
            |pos| {
                let date = stats.calendar_start.plus_days(day(pos) as i64);
                format!("{} · {}", day_label(date), totals(stats.days[day(pos)]))
            },
        );
        ui.add_space(4.0);
        weak(ui, text);
        ui.add_space(22.0);

        caption(ui, "TIME OF DAY");
        let cell = |(col, row): (usize, usize)| stats.time_of_day[row][col];
        let max = stats.time_of_day.iter().flatten().map(Bucket::total).max();
        let hour_labels = [(0, "0"), (6, "6"), (12, "12"), (18, "18")]
            .map(|(c, t)| (c, t.to_string()))
            .to_vec();
        ui.horizontal_top(|ui| {
            let width = LABEL_WIDTH + 24.0 * (18.0 + GAP);
            let hovered = ui
                .allocate_ui(vec2(width, 0.0), |ui| {
                    heatmap(
                        ui,
                        (24, 7),
                        18.0,
                        &hour_labels,
                        &|_| true,
                        &|pos| cell(pos).total(),
                        max.unwrap_or(0),
                        &mut self.pinned_hour,
                    )
                })
                .inner;
            ui.add_space(24.0);
            ui.vertical(|ui| {
                ui.add_space(LABEL_HEIGHT);
                match hovered.or(self.pinned_hour) {
                    Some((h, d)) => {
                        let b = cell((h, d));
                        weak(ui, format!("{} {h:02}:00–{:02}:00", WEEKDAYS[d], h + 1));
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(
                                RichText::new(group(b.total()))
                                    .size(20.0)
                                    .color(BRAND_COTTONTAIL),
                            );
                            ui.label("inputs");
                        });
                        for line in breakdown_lines(b) {
                            weak(ui, line);
                        }
                    }
                    None => weak(ui, "Hover or click an hour for its counts".into()),
                }
            });
        });
    }
}

fn settings_menu(ui: &mut egui::Ui) {
    let response = gear_button(ui);
    egui::Popup::from_toggle_button_response(&response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(RectAlign::BOTTOM_END)
        .gap(6.0)
        .show(|ui| {
            ui.set_min_width(220.0);
            caption(ui, "SETTINGS");
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
            let mut rabbit = settings::read(|s| s.overlay);
            if ui
                .checkbox(&mut rabbit, "Corner rabbit on screen")
                .changed()
            {
                settings::update(|s| s.overlay = rabbit);
                overlay::sync();
                tray::sync_menu();
            }
        });
}

fn gear_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
    let color = if response.hovered() {
        BRAND_COTTONTAIL
    } else {
        BRAND_FG_WEAK
    };
    if response.hovered() {
        ui.painter().rect_filled(rect, 6.0, BRAND_BG_LIGHT);
    }
    let c = rect.center();
    let stroke = Stroke::new(1.6, color);
    let painter = ui.painter();
    for i in 0..8 {
        let dir = Vec2::angled(i as f32 * std::f32::consts::TAU / 8.0);
        painter.line_segment([c + dir * 6.5, c + dir * 9.0], Stroke::new(2.6, color));
    }
    painter.circle_stroke(c, 6.0, stroke);
    painter.circle_stroke(c, 2.4, stroke);
    response.on_hover_text("Settings")
}

fn tab_button(ui: &mut egui::Ui, selected: bool, text: &str) -> egui::Response {
    let font = FontId::proportional(15.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.into(), font, Color32::PLACEHOLDER);
    let pad = vec2(14.0, 10.0);
    let (rect, response) =
        ui.allocate_exact_size(galley.size() + pad * 2.0 + vec2(0.0, 2.0), Sense::click());
    let color = if selected {
        BRAND_COTTONTAIL
    } else if response.hovered() {
        BRAND_FG
    } else {
        BRAND_FG_WEAK
    };
    ui.painter().galley(rect.min + pad, galley, color);
    if selected {
        let line = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 2.0), rect.max);
        ui.painter().rect_filled(line, 0.0, BRAND_PRIMARY);
    }
    response
}

pub(crate) fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str)],
) {
    egui::Frame::new()
        .fill(BRAND_DARKER)
        .corner_radius(7)
        .inner_margin(3)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.spacing_mut().button_padding = vec2(12.0, 4.0);
                // Inside a right-aligned row egui lays out right to left; keep reading order.
                let mirrored = ui.layout().prefer_right_to_left();
                let ordered: Vec<_> = if mirrored {
                    options.iter().rev().collect()
                } else {
                    options.iter().collect()
                };
                for (option, label) in ordered {
                    let on = *value == *option;
                    let text =
                        RichText::new(*label).color(if on { BRAND_COTTONTAIL } else { BRAND_FG });
                    let button = egui::Button::new(text)
                        .fill(if on {
                            BRAND_BG_LIGHTEST
                        } else {
                            Color32::TRANSPARENT
                        })
                        .corner_radius(5);
                    if ui.add(button).clicked() {
                        *value = *option;
                    }
                }
            });
        });
}

fn footer(ui: &mut egui::Ui) {
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

pub(crate) fn caption(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().strong().color(BRAND_FG_WEAK));
    ui.add_space(2.0);
}

pub(crate) fn weak(ui: &mut egui::Ui, text: String) {
    ui.label(RichText::new(text).small().color(BRAND_FG_WEAK));
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(BRAND_BG_LIGHT)
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn stat(ui: &mut egui::Ui, value: u64, label: &str, previous: String) {
    let text = group(value);
    let size = if text.len() > 9 { 22.0 } else { 28.0 };
    card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(RichText::new(text).size(size).color(BRAND_COTTONTAIL));
        ui.label(label);
        weak(ui, previous);
    });
}

fn all_time(ui: &mut egui::Ui, total: Bucket, revealed: &mut bool) {
    let value = |ui: &mut egui::Ui, n: u64, noun: &str| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new(group(n)).color(BRAND_COTTONTAIL));
            ui.label(noun);
        });
    };
    egui::Grid::new("all-time")
        .num_columns(2)
        .spacing(vec2(28.0, 4.0))
        .show(ui, |ui| {
            value(ui, total.keys, "keys");
            value(ui, total.clicks, "clicks");
            ui.end_row();
            value(ui, total.scrolls, "scrolls");
            weak(ui, format!("{} scrolled", scroll_distance(total.scrolls)));
            ui.end_row();
            if total.has_controller() {
                value(ui, total.buttons, "buttons");
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label("LT");
                    ui.label(RichText::new(group(total.lt_pulls)).color(BRAND_COTTONTAIL));
                    ui.label("· RT");
                    ui.label(RichText::new(group(total.rt_pulls)).color(BRAND_COTTONTAIL));
                });
                ui.end_row();
            }
        });
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label("Gross total");
        ui.add_space(4.0);
        if *revealed {
            let number = RichText::new(group(total.total()))
                .size(18.0)
                .color(BRAND_COTTONTAIL);
            if ui
                .add(egui::Label::new(number).sense(Sense::click()))
                .on_hover_text("Click to hide")
                .clicked()
            {
                *revealed = false;
            }
        } else if ui.button("tap to reveal").clicked() {
            *revealed = true;
        }
    });
}

fn controller_table(ui: &mut egui::Ui, c: Bucket, p: Bucket, before: &str) {
    egui::Grid::new("controller")
        .num_columns(4)
        .spacing(vec2(16.0, 6.0))
        .show(ui, |ui| {
            ui.label("Buttons");
            ui.label(RichText::new(group(c.buttons)).color(BRAND_COTTONTAIL));
            ui.label("");
            weak(ui, format!("{before} {}", group(p.buttons)));
            ui.end_row();
            for (name, pulls, ms, prev) in [
                ("LT", c.lt_pulls, c.lt_ms, p.lt_pulls),
                ("RT", c.rt_pulls, c.rt_ms, p.rt_pulls),
            ] {
                ui.label(name);
                ui.label(RichText::new(format!("{} pulls", group(pulls))).color(BRAND_COTTONTAIL));
                ui.label(format!("held {}", duration(ms)));
                weak(ui, format!("{before} {}", group(prev)));
                ui.end_row();
            }
        });
}

fn breakdown(c: Bucket) -> String {
    breakdown_lines(c).join(" · ")
}

fn breakdown_lines(c: Bucket) -> Vec<String> {
    let mut lines = vec![format!(
        "{} keys · {} clicks · {} scrolls",
        group(c.keys),
        group(c.clicks),
        group(c.scrolls)
    )];
    if c.has_controller() {
        lines.push(format!(
            "{} buttons · LT {} · RT {}",
            group(c.buttons),
            group(c.lt_pulls),
            group(c.rt_pulls)
        ));
    }
    lines
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

/// Square cells, columns left to right, rows top to bottom (Monday first), as wide as fits up to
/// `max_cell`. Returns the hovered cell; a click pins a cell, a second click on it unpins.
#[allow(clippy::too_many_arguments)]
fn heatmap(
    ui: &mut egui::Ui,
    (cols, rows): (usize, usize),
    max_cell: f32,
    col_labels: &[(usize, String)],
    exists: &dyn Fn((usize, usize)) -> bool,
    value: &dyn Fn((usize, usize)) -> u64,
    max: u64,
    pinned: &mut Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    let cell = ((ui.available_width() - LABEL_WIDTH) / cols as f32 - GAP).clamp(4.0, max_cell);
    let step = cell + GAP;
    let size = vec2(
        LABEL_WIDTH + cols as f32 * step,
        LABEL_HEIGHT + rows as f32 * step,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let origin = rect.min + vec2(LABEL_WIDTH, LABEL_HEIGHT);
    let cell_at = |p: egui::Pos2| {
        let rel = p - origin;
        let (col, row) = ((rel.x / step).floor(), (rel.y / step).floor());
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

    let painter = ui.painter_at(rect.expand(2.0));
    let small = FontId::proportional(11.0);
    let every_row = cell >= 14.0;
    for (row, name) in WEEKDAYS.iter().enumerate().take(rows) {
        if every_row || row % 2 == 0 {
            painter.text(
                pos2(rect.left(), origin.y + row as f32 * step + cell / 2.0),
                Align2::LEFT_CENTER,
                *name,
                small.clone(),
                BRAND_FG_WEAK,
            );
        }
    }
    for (col, text) in col_labels {
        painter.text(
            pos2(origin.x + *col as f32 * step, rect.top()),
            Align2::LEFT_TOP,
            text,
            small.clone(),
            BRAND_FG_WEAK,
        );
    }
    let radius = (cell / 5.0).clamp(2.0, 3.0);
    for col in 0..cols {
        for row in 0..rows {
            if !exists((col, row)) {
                continue;
            }
            let min = origin + vec2(col as f32 * step, row as f32 * step);
            let r = Rect::from_min_size(min, Vec2::splat(cell));
            painter.rect_filled(r, radius, heat(value((col, row)), max));
            if *pinned == Some((col, row)) || hovered == Some((col, row)) {
                painter.rect_stroke(
                    r.expand(1.0),
                    radius,
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
    v.window_fill = BRAND_BG_LIGHT;
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
        state.corner_radius = CornerRadius::same(5);
        state.expansion = 0.0;
        state.bg_stroke = Stroke::NONE;
        state.fg_stroke = Stroke::new(1.0, BRAND_FG);
    }
    w.inactive.bg_fill = BRAND_BG_LIGHTEST;
    w.inactive.weak_bg_fill = BRAND_BG_LIGHTEST;
    w.hovered.bg_fill = BRAND_BG_LIGHTEST;
    w.hovered.fg_stroke = Stroke::new(1.5, BRAND_COTTONTAIL);
    w.active.bg_fill = BRAND_BG_LIGHTEST;
    w.active.fg_stroke = Stroke::new(1.5, BRAND_COTTONTAIL);
    ctx.set_visuals(v);

    ctx.global_style_mut(|style| {
        let font = |size| FontId::new(size, FontFamily::Proportional);
        style.text_styles = [
            (TextStyle::Small, font(12.0)),
            (TextStyle::Body, font(14.0)),
            (TextStyle::Button, font(14.0)),
            (TextStyle::Heading, font(22.0)),
            (
                TextStyle::Monospace,
                FontId::new(13.0, FontFamily::Monospace),
            ),
        ]
        .into();
        style.spacing.button_padding = vec2(10.0, 4.0);
        style.spacing.item_spacing = vec2(8.0, 6.0);
    });
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

    #[test]
    fn taps_drum_their_paws() {
        let zero = Bucket::default();
        let mut alt = false;
        let click = |f: fn(&mut Bucket)| {
            let mut b = zero;
            f(&mut b);
            b
        };
        assert_eq!(drum_sides(zero, click(|b| b.m_left = 1), &mut alt), LEFT);
        assert_eq!(drum_sides(zero, click(|b| b.m_right = 1), &mut alt), RIGHT);
        assert_eq!(drum_sides(zero, click(|b| b.scrolls = 2), &mut alt), BOTH);
        assert_eq!(drum_sides(zero, click(|b| b.keys = 1), &mut alt), LEFT);
        assert_eq!(drum_sides(zero, click(|b| b.keys = 1), &mut alt), RIGHT);
        assert_eq!(drum_sides(zero, click(|b| b.keys = 3), &mut alt), BOTH);
        assert_eq!(
            drum_sides(click(|b| b.keys = 5), zero, &mut alt),
            0,
            "flushed"
        );
        assert_eq!(drum_sides(zero, zero, &mut alt), 0);
    }

    #[test]
    fn paws_lift_after_the_hold() {
        let mut paws = Paws::default();
        assert_eq!(paws.frame(0.0, false), 0);
        paws.hit(LEFT, 1.0);
        assert_eq!(paws.frame(1.05, false), 1);
        paws.hit(RIGHT, 1.05);
        assert_eq!(paws.frame(1.06, false), 3);
        assert_eq!(paws.frame(1.06, true), 4);
        assert_eq!(paws.next_change(1.06), Some(1.0 + PAW_HOLD));
        assert_eq!(paws.frame(1.2, false), 0);
        assert_eq!(paws.next_change(1.2), None);
    }
}
