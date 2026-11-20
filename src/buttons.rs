use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Id, Rect, RichText, Sense, Stroke, StrokeKind,
    Vec2, pos2, vec2,
};

use crate::counts::{Bucket, duration, group};
use crate::keyboard::{self, Counts, Shape};
use crate::layout::{self, BOARD_HEIGHT, BOARD_WIDTH, Key, Layout};
use crate::settings;
use crate::stats::{Period, Stats};
use crate::ui::{
    BRAND_BG_LIGHT, BRAND_BG_LIGHTEST, BRAND_COTTONTAIL, BRAND_DARKER, BRAND_FG, BRAND_FG_WEAK,
    Fingerprint, heat, segmented, weak,
};

const KEYS_LINE: &str =
    "Counts presses per key position. Never what you typed, the order, or when.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Device {
    Keyboard,
    Mouse,
    Controller,
}

struct Part {
    rect: Rect,
    /// Corner radii (nw, ne, sw, se) in diagram units, capped at half the shorter side.
    corners: [f32; 4],
    label: String,
    value: u64,
    detail: String,
}

struct Board {
    shape: Shape,
    layout: Layout,
    keys: Vec<Key>,
    parts: Vec<Part>,
    names: Vec<String>,
    total: u64,
    counted: Option<Fingerprint>,
}

pub struct Buttons {
    device: Device,
    /// `None` is all time.
    range: Option<Period>,
    pinned: Option<usize>,
    confirm_off: bool,
    board: Option<Board>,
    layouts: Vec<(Layout, String)>,
    shape_override: Option<Shape>,
    layout_override: Option<Layout>,
    /// Installed layouts and overrides are re-read only when the view changes.
    checked: Option<Fingerprint>,
}

impl Buttons {
    pub fn new() -> Self {
        Self {
            device: Device::Keyboard,
            range: None,
            pinned: None,
            confirm_off: false,
            board: None,
            layouts: Vec::new(),
            shape_override: None,
            layout_override: None,
            checked: None,
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        device: Device,
        stats: &Stats,
        keys: &Counts,
        key: Fingerprint,
    ) {
        if self.device != device {
            self.device = device;
            self.pinned = None;
        }
        match self.device {
            Device::Keyboard => self.keyboard(ui, keys, key),
            Device::Mouse | Device::Controller => {
                self.range_picker(ui);
                let b = match self.range {
                    Some(p) => stats.period(p).0,
                    None => stats.total,
                };
                ui.add_space(6.0);
                if self.device == Device::Mouse {
                    let parts = mouse(b);
                    let hovered =
                        diagram(ui, MOUSE_UNITS, 22.0, &parts, &MOUSE_BODY, &mut self.pinned);
                    self.detail(ui, &parts, hovered, "Hover or click a button for its count");
                } else {
                    let parts = controller(b);
                    let hovered = diagram(ui, PAD_UNITS, 26.0, &parts, &PAD_BODY, &mut self.pinned);
                    if b.has_controller() || stats.total.has_controller() {
                        self.detail(ui, &parts, hovered, "Hover or click a button for its count");
                    } else {
                        weak(
                            ui,
                            "No controller input yet. Xbox controllers count once connected."
                                .into(),
                        );
                    }
                }
            }
        }
    }

    fn range_picker(&mut self, ui: &mut egui::Ui) {
        let mut options: Vec<_> = Period::ALL.map(|p| (Some(p), p.label())).into();
        options.push((None, "All time"));
        segmented(ui, &mut self.range, &options);
    }

    fn detail(&self, ui: &mut egui::Ui, parts: &[Part], hovered: Option<usize>, hint: &str) {
        let text = hovered
            .or(self.pinned)
            .and_then(|i| parts.get(i))
            .map_or_else(|| hint.to_string(), |p| p.detail.clone());
        weak(ui, text);
    }

    fn keyboard(&mut self, ui: &mut egui::Ui, counts: &Counts, key: Fingerprint) {
        let mut on = keyboard::enabled();
        if ui.checkbox(&mut on, "Count presses per key").changed() {
            if on {
                keyboard::turn_on();
            } else {
                self.confirm_off = true;
            }
        }
        weak(ui, KEYS_LINE.into());
        if self.confirm_off {
            self.confirm_off_dialog(ui.ctx());
        }
        if !keyboard::enabled() {
            return;
        }
        ui.add_space(6.0);
        self.layout_pickers(ui, key);
        ui.add_space(6.0);

        let board = self.board.as_mut().expect("layout_pickers sets the board");
        if board.counted != Some(key) {
            board.counted = Some(key);
            board.total = 0;
            for ((part, k), name) in board.parts.iter_mut().zip(&board.keys).zip(&board.names) {
                part.value = counts[k.slot] + k.alias.map_or(0, |a| counts[a]);
                part.detail = presses(name, part.value);
                board.total += part.value;
            }
        }
        let hovered = diagram(
            ui,
            vec2(BOARD_WIDTH, BOARD_HEIGHT),
            40.0,
            &board.parts,
            &[],
            &mut self.pinned,
        );
        let hint = format!(
            "{} presses on this board, all time. Hover or click a key for its count.",
            group(board.total)
        );
        let board = self.board.as_ref().expect("set above");
        self.detail(ui, &board.parts, hovered, &hint);
    }

    fn refresh_layouts(&mut self) {
        let installed = Layout::installed();
        if self.layouts.len() != installed.len()
            || self
                .layouts
                .iter()
                .zip(&installed)
                .any(|((a, _), b)| a != b)
        {
            self.layouts = installed.into_iter().map(|l| (l, l.name())).collect();
        }
        let (shape, layout) = settings::read(|s| {
            (
                s.key_shape,
                s.key_layout.as_deref().and_then(Layout::from_setting),
            )
        });
        self.shape_override = shape;
        self.layout_override = layout.filter(|l| self.layouts.iter().any(|(x, _)| x == l));
    }

    /// Rebuilds the board only when its shape or labels changed.
    fn layout_pickers(&mut self, ui: &mut egui::Ui, key: Fingerprint) {
        if self.checked != Some(key) {
            self.checked = Some(key);
            self.refresh_layouts();
        }
        // Every frame: Windows applies a layout switch on key release, which moves no count.
        let active = Layout::active();
        let guessed = keyboard::guess_shape(self.layout_override.unwrap_or(active).langid());
        let name = |l: Layout| {
            self.layouts
                .iter()
                .find(|(x, _)| *x == l)
                .map_or_else(|| l.name(), |(_, n)| n.clone())
        };
        let mut shape_choice = self.shape_override;
        let mut layout_choice = self.layout_override;
        ui.horizontal(|ui| {
            ui.label("Shape");
            egui::ComboBox::from_id_salt("key-shape")
                .selected_text(shape_choice.map_or_else(
                    || format!("Auto ({})", guessed.label()),
                    |s| s.label().into(),
                ))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut shape_choice,
                        None,
                        format!("Auto ({})", guessed.label()),
                    );
                    for s in Shape::ALL {
                        ui.selectable_value(&mut shape_choice, Some(s), s.label());
                    }
                });
            ui.label("Labels");
            egui::ComboBox::from_id_salt("key-labels")
                .selected_text(layout_choice.map_or_else(|| "Auto".to_string(), name))
                .width(ui.available_width() - 8.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut layout_choice,
                        None,
                        format!("Auto ({})", name(active)),
                    );
                    for (l, n) in &self.layouts {
                        ui.selectable_value(&mut layout_choice, Some(*l), n);
                    }
                });
        });
        if shape_choice != self.shape_override || layout_choice != self.layout_override {
            settings::update(|s| {
                s.key_shape = shape_choice;
                s.key_layout = layout_choice.map(Layout::to_setting);
            });
            self.shape_override = shape_choice;
            self.layout_override = layout_choice;
            self.pinned = None;
        }
        let layout = layout_choice.unwrap_or(active);
        let shape = shape_choice.unwrap_or_else(|| keyboard::guess_shape(layout.langid()));
        if self
            .board
            .as_ref()
            .is_none_or(|b| b.shape != shape || b.layout != layout)
        {
            let keys = layout::board(shape);
            let labels = layout::labels(&keys, layout);
            let names = layout::names(&keys, &labels);
            let parts = keys
                .iter()
                .zip(labels)
                .map(|(k, label)| Part {
                    rect: Rect::from_min_size(pos2(k.x, k.y), vec2(k.w, k.h)),
                    corners: [KEY_CORNER; 4],
                    label,
                    value: 0,
                    detail: String::new(),
                })
                .collect();
            self.board = Some(Board {
                shape,
                layout,
                keys,
                parts,
                names,
                total: 0,
                counted: None,
            });
        }
    }

    fn confirm_off_dialog(&mut self, ctx: &egui::Context) {
        let response = egui::Modal::new(Id::unique("keys-off")).show(ctx, |ui| {
            ui.set_width(280.0);
            ui.label(
                RichText::new("Turn off the keyboard heatmap?")
                    .strong()
                    .color(BRAND_COTTONTAIL),
            );
            ui.label("Delete the per-key counts stored so far too?");
            ui.add_space(8.0);
            let mut done = false;
            ui.horizontal(|ui| {
                if ui.button("Turn off and delete").clicked() {
                    keyboard::turn_off(true);
                    done = true;
                }
                if ui.button("Turn off, keep").clicked() {
                    keyboard::turn_off(false);
                    done = true;
                }
                if ui.button("Cancel").clicked() {
                    done = true;
                }
            });
            done
        });
        if response.inner || response.should_close() {
            self.confirm_off = false;
            self.pinned = None;
        }
    }
}

const KEY_CORNER: f32 = 0.12;
const ROUND: [f32; 4] = [f32::INFINITY; 4];

/// `body` shapes are (rect, corner radius) in the same units as the parts, drawn as one outline.
/// A click pins a part.
fn diagram(
    ui: &mut egui::Ui,
    units: Vec2,
    max_scale: f32,
    parts: &[Part],
    body: &[(Rect, f32)],
    pinned: &mut Option<usize>,
) -> Option<usize> {
    let width = ui.available_width();
    let scale = (width / units.x).min(max_scale);
    let size = units * scale;
    let (rect, response) = ui.allocate_exact_size(vec2(width, size.y), Sense::click());
    let origin = pos2(rect.center().x - size.x / 2.0, rect.top());
    let map = |r: Rect| {
        Rect::from_min_max(
            origin + r.min.to_vec2() * scale,
            origin + r.max.to_vec2() * scale,
        )
    };
    let hit = |p| parts.iter().position(|part| map(part.rect).contains(p));
    let hovered = response.hover_pos().and_then(hit);
    if response.clicked()
        && let Some(i) = response.interact_pointer_pos().and_then(hit)
    {
        *pinned = if *pinned == Some(i) { None } else { Some(i) };
    }

    let painter = ui.painter_at(rect);
    for (fill, grow) in [(BRAND_BG_LIGHTEST, 1.0), (BRAND_DARKER, 0.0)] {
        for (r, radius) in body {
            painter.rect_filled(map(*r).expand(grow), radius * scale + grow, fill);
        }
    }
    let max = parts.iter().map(|p| p.value).max().unwrap_or(0);
    let inset = (scale * 0.06).clamp(1.0, 3.0);
    for (i, part) in parts.iter().enumerate() {
        let r = map(part.rect).shrink(inset);
        let cap = r.width().min(r.height()) / 2.0;
        let [nw, ne, sw, se] = part.corners.map(|c| (c * scale).min(cap).min(255.0) as u8);
        let corners = CornerRadius { nw, ne, sw, se };
        let fill = heat(part.value, max);
        painter.rect_filled(r, corners, fill);
        if *pinned == Some(i) || hovered == Some(i) {
            painter.rect_stroke(
                r,
                corners,
                Stroke::new(1.5, BRAND_COTTONTAIL),
                StrokeKind::Outside,
            );
        }
        let lines = part.label.lines().count().max(1) as f32;
        let longest = part
            .label
            .lines()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0)
            .max(1) as f32;
        let size = (r.height() * 0.5 / lines)
            .min(r.width() * 1.7 / longest)
            .clamp(6.0, 14.0);
        if let Some(dir) = arrow(&part.label) {
            let c = r.center();
            let s = size * 0.45;
            let tip = c + dir * s;
            let side = vec2(-dir.y, dir.x) * s;
            let back = c - dir * s;
            painter.add(egui::Shape::convex_polygon(
                vec![tip, back + side, back - side],
                label_color(fill),
                Stroke::NONE,
            ));
            continue;
        }
        painter.text(
            r.center(),
            Align2::CENTER_CENTER,
            &part.label,
            FontId::proportional(size),
            label_color(fill),
        );
    }
    hovered
}

fn label_color(fill: Color32) -> Color32 {
    let [r, g, b, _] = fill.to_array();
    if u32::from(r) + u32::from(g) + u32::from(b) > 330 {
        BRAND_DARKER
    } else if fill == BRAND_BG_LIGHT {
        BRAND_FG_WEAK
    } else {
        BRAND_FG
    }
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
    Rect::from_min_max(pos2(x0, y0), pos2(x1, y1))
}

fn circle(cx: f32, cy: f32, r: f32) -> Rect {
    rect(cx - r, cy - r, cx + r, cy + r)
}

fn presses(name: &str, n: u64) -> String {
    format!("{name}: {} presses", group(n))
}

fn part(rect: Rect, corners: [f32; 4], label: &str, name: &str, value: u64) -> Part {
    Part {
        rect,
        corners,
        label: label.into(),
        value,
        detail: presses(name, value),
    }
}

const MOUSE_UNITS: Vec2 = vec2(10.0, 14.0);
const MOUSE_BODY: [(Rect, f32); 1] = [(
    Rect {
        min: pos2(1.0, 0.4),
        max: pos2(9.0, 13.6),
    },
    3.6,
)];

fn mouse(b: Bucket) -> Vec<Part> {
    let (top, edge) = (3.3, 0.2);
    let main = |r, corners, short: &str, name: &str, value: u64| Part {
        label: format!("{short}\n{}", group(value)),
        ..part(r, corners, "", name, value)
    };
    vec![
        main(
            rect(1.25, 0.65, 4.95, 6.0),
            [top, edge, edge, edge],
            "Left",
            "Left button",
            b.m_left,
        ),
        main(
            rect(5.05, 0.65, 8.75, 6.0),
            [edge, top, edge, edge],
            "Right",
            "Right button",
            b.m_right,
        ),
        part(
            rect(4.4, 1.6, 5.6, 4.6),
            ROUND,
            "M",
            "Middle button (wheel)",
            b.m_middle,
        ),
        part(
            rect(0.4, 6.6, 1.6, 8.4),
            [0.4; 4],
            "X2",
            "Forward button (X2)",
            b.m_x2,
        ),
        part(
            rect(0.4, 8.6, 1.6, 10.4),
            [0.4; 4],
            "X1",
            "Back button (X1)",
            b.m_x1,
        ),
    ]
}

const PAD_UNITS: Vec2 = vec2(20.0, 12.0);
const PAD_BODY: [(Rect, f32); 4] = [
    (
        Rect {
            min: pos2(2.0, 2.6),
            max: pos2(18.0, 8.8),
        },
        3.0,
    ),
    (
        Rect {
            min: pos2(1.6, 4.5),
            max: pos2(7.4, 11.8),
        },
        2.9,
    ),
    (
        Rect {
            min: pos2(12.6, 4.5),
            max: pos2(18.4, 11.8),
        },
        2.9,
    ),
    // The d-pad hub, so its four arms read as one cross.
    (
        Rect {
            min: pos2(7.0, 7.5),
            max: pos2(8.0, 8.5),
        },
        0.0,
    ),
];

fn controller(b: Bucket) -> Vec<Part> {
    let trigger = |r: Rect, name: &str, pulls: u64, ms: u64| Part {
        rect: r,
        corners: [0.6, 0.6, 0.15, 0.15],
        label: format!("{name} {}", duration(ms)),
        value: pulls,
        detail: format!("{name}: {} pulls, held {}", group(pulls), duration(ms)),
    };
    let bumper = [0.5, 0.5, 0.15, 0.15];
    let arm = [0.15; 4];
    vec![
        trigger(rect(2.4, 0.0, 6.8, 1.2), "LT", b.lt_pulls, b.lt_ms),
        trigger(rect(13.2, 0.0, 17.6, 1.2), "RT", b.rt_pulls, b.rt_ms),
        part(
            rect(2.6, 1.4, 6.8, 2.4),
            bumper,
            "LB",
            "Left bumper",
            b.pad_lb,
        ),
        part(
            rect(13.2, 1.4, 17.4, 2.4),
            bumper,
            "RB",
            "Right bumper",
            b.pad_rb,
        ),
        part(
            circle(5.0, 5.0, 1.4),
            ROUND,
            "LS",
            "Left stick click",
            b.pad_ls,
        ),
        part(
            circle(12.6, 7.9, 1.2),
            ROUND,
            "RS",
            "Right stick click",
            b.pad_rs,
        ),
        part(rect(7.0, 6.5, 8.0, 7.5), arm, "↑", "D-pad up", b.pad_up),
        part(rect(7.0, 8.5, 8.0, 9.5), arm, "↓", "D-pad down", b.pad_down),
        part(rect(6.0, 7.5, 7.0, 8.5), arm, "←", "D-pad left", b.pad_left),
        part(
            rect(8.0, 7.5, 9.0, 8.5),
            arm,
            "→",
            "D-pad right",
            b.pad_right,
        ),
        part(rect(8.6, 4.4, 9.7, 5.2), ROUND, "", "Back", b.pad_back),
        part(rect(10.3, 4.4, 11.4, 5.2), ROUND, "", "Start", b.pad_start),
        part(circle(15.0, 3.9, 0.6), ROUND, "Y", "Y", b.pad_y),
        part(circle(13.8, 5.1, 0.6), ROUND, "X", "X", b.pad_x),
        part(circle(16.2, 5.1, 0.6), ROUND, "B", "B", b.pad_b),
        part(circle(15.0, 6.3, 0.6), ROUND, "A", "A", b.pad_a),
    ]
}

/// The default fonts have no arrow glyphs, so arrow legends are drawn as triangles.
fn arrow(label: &str) -> Option<Vec2> {
    match label {
        "↑" => Some(vec2(0.0, -1.0)),
        "↓" => Some(vec2(0.0, 1.0)),
        "←" => Some(vec2(-1.0, 0.0)),
        "→" => Some(vec2(1.0, 0.0)),
        _ => None,
    }
}
