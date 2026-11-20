use eframe::egui::{self, Align, Layout, RichText};

use super::charts::{
    hand_split, legend, ranked_rows, recent_grid, trend, typical_strip, weekday_bars,
};
use super::{BRAND_COTTONTAIL, MONTHS, WEEKDAYS, caption, card, day_label, group, segmented, weak};
use crate::clock::Date;
use crate::stats::{DAY_START, KeyInsights, RANGES, Stats, kcs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Trend,
    Habits,
    Records,
    Keyboard,
}

pub struct StatsTab {
    range: usize,
    page: Page,
}

pub struct KeyView {
    pub insights: KeyInsights,
    pub names: Vec<String>,
}

const RANGE_LABELS: [&str; 3] = ["30 days", "90 days", "365 days"];
const PANEL_HEIGHT: f32 = 150.0;

impl StatsTab {
    pub fn new() -> Self {
        Self {
            range: 0,
            page: Page::Trend,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, stats: &Stats, keys: Option<&KeyView>) {
        if keys.is_none() && self.page == Page::Keyboard {
            self.page = Page::Trend;
        }
        ui.horizontal(|ui| {
            let mut options = vec![
                (Page::Trend, "Trend"),
                (Page::Habits, "Habits"),
                (Page::Records, "Records"),
            ];
            if keys.is_some() {
                options.push((Page::Keyboard, "Keyboard"));
            }
            segmented(ui, &mut self.page, &options);
            if matches!(self.page, Page::Trend | Page::Habits) {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let options: Vec<_> = (0..RANGES.len()).map(|i| (i, RANGE_LABELS[i])).collect();
                    segmented(ui, &mut self.range, &options);
                });
            }
        });
        ui.add_space(12.0);
        match self.page {
            Page::Trend => self.trend(ui, stats),
            Page::Habits => self.habits(ui, stats),
            Page::Records => records(ui, stats),
            Page::Keyboard => {
                if let Some(k) = keys {
                    keyboard(ui, k);
                }
            }
        }
    }

    fn trend(&self, ui: &mut egui::Ui, stats: &Stats) {
        let days = RANGES[self.range];
        let today = (stats.now.date.to_days() - stats.calendar_start.to_days()) as usize;
        let from = (today + 1).saturating_sub(days);
        let window = &stats.days[from..=today];
        let sum: u64 = window.iter().map(kcs).sum();
        let best = window
            .iter()
            .enumerate()
            .max_by_key(|(_, b)| kcs(b))
            .filter(|(_, b)| kcs(b) > 0);
        ui.columns(3, |cols| {
            tile(
                &mut cols[0],
                group(sum),
                "inputs",
                format!("in the last {days} days"),
            );
            tile(
                &mut cols[1],
                group(sum / days as u64),
                "a day",
                "on average".into(),
            );
            tile(
                &mut cols[2],
                group(best.map_or(0, |(_, b)| kcs(b))),
                "best day",
                best.map_or_else(String::new, |(i, _)| {
                    day_label(stats.calendar_start.plus_days((from + i) as i64))
                }),
            );
        });
        ui.add_space(12.0);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                caption(ui, "DAILY TOTALS");
                ui.add_space(12.0);
                legend(ui);
            });
            let start = stats.calendar_start.plus_days(from as i64);
            let hovered = trend(ui, 170.0, window, &stats.avg7[from..=today], start);
            ui.add_space(4.0);
            weak(
                ui,
                match hovered {
                    Some(i) => {
                        let b = window[i];
                        format!(
                            "{} · {} keys · {} clicks · {} scrolls",
                            day_label(start.plus_days(i as i64)),
                            group(b.keys),
                            group(b.clicks),
                            group(b.scrolls)
                        )
                    }
                    None => "Line: 7-day average. Hover a day for its counts.".into(),
                },
            );
        });
    }

    fn habits(&self, ui: &mut egui::Ui, stats: &Stats) {
        let t = &stats.typical[self.range];
        card(ui, |ui| {
            caption(
                ui,
                &format!("A TYPICAL DAY · LAST {} DAYS", RANGES[self.range]),
            );
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                match t.usual {
                    Some((first, end)) => {
                        ui.label(RichText::new("Usually").size(18.0));
                        ui.label(
                            RichText::new(format!("{} – {}", clock(first), clock(end)))
                                .size(18.0)
                                .color(BRAND_COTTONTAIL),
                        );
                        weak(
                            ui,
                            format!(
                                "· {:.1} h with input on an active day ({} of {} days)",
                                t.mean_hours, t.active_days, RANGES[self.range]
                            ),
                        );
                    }
                    None => weak(ui, "No input in this range yet".into()),
                }
            });
            let hovered = typical_strip(ui, &t.share, t.usual);
            ui.add_space(2.0);
            weak(
                ui,
                match hovered {
                    Some(h) => format!(
                        "{}–{} · input on {:.0}% of active days",
                        clock(h as u8),
                        clock(h as u8 + 1),
                        t.share[h] * 100.0
                    ),
                    None => "Brighter hours had input on more days; the line marks the usual start and end".into(),
                },
            );
        });
        ui.add_space(12.0);
        ui.columns(2, |cols| {
            card(&mut cols[0], |ui| {
                ui.set_min_height(PANEL_HEIGHT);
                caption(ui, "LAST 7 DAYS");
                let shape_today = stats.now.plus(-i64::from(DAY_START)).date;
                let start = shape_today.minus_days(stats.recent.len() as i64 - 1);
                let hovered = recent_grid(ui, &stats.recent, start);
                weak(
                    ui,
                    match hovered {
                        Some((d, h)) => format!(
                            "{} {}–{} · {} inputs",
                            short_day(start.plus_days(d as i64)),
                            clock(h as u8),
                            clock(h as u8 + 1),
                            group(stats.recent[d][h])
                        ),
                        None => "Each row one day, hours with input lit".into(),
                    },
                );
            });
            card(&mut cols[1], |ui| {
                ui.set_min_height(PANEL_HEIGHT);
                caption(ui, "AVERAGE BY WEEKDAY");
                let hovered = weekday_bars(ui, 110.0, &stats.weekdays);
                weak(ui, weekday_readout(stats, hovered));
            });
        });
    }
}

fn clock(from_start: u8) -> String {
    format!("{:02}:00", (u32::from(from_start) + DAY_START) % 24)
}

fn short_day(d: Date) -> String {
    format!("{} {}", WEEKDAYS[d.weekday()], d.day)
}

fn short_date(d: Date) -> String {
    format!("{} {} {}", d.day, MONTHS[d.month as usize - 1], d.year)
}

fn days(n: u32) -> String {
    if n == 1 {
        "1 day".into()
    } else {
        format!("{n} days")
    }
}

fn percent(part: u64, total: u64) -> String {
    format!("{:.1}%", part as f64 * 100.0 / total.max(1) as f64)
}

fn weekday_readout(stats: &Stats, hovered: Option<usize>) -> String {
    match hovered {
        Some(i) => {
            let [k, c, s] = stats.weekdays[i];
            format!(
                "{} · {} keys · {} clicks · {} scrolls on average",
                WEEKDAYS[i],
                group(k as u64),
                group(c as u64),
                group(s as u64)
            )
        }
        None => "Hover a day for its averages".into(),
    }
}

fn tile(ui: &mut egui::Ui, value: String, label: &str, sub: String) {
    card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(RichText::new(value).size(24.0).color(BRAND_COTTONTAIL));
        ui.label(label);
        weak(ui, sub);
    });
}

fn records(ui: &mut egui::Ui, stats: &Stats) {
    let r = stats.records;
    let when = |v: Option<String>| v.unwrap_or_else(|| "no input yet".into());
    ui.columns(3, |cols| {
        tile(
            &mut cols[0],
            group(r.hour.map_or(0, |h| h.1)),
            "busiest hour",
            when(
                r.hour
                    .map(|(h, _)| format!("{} {:02}:00", day_label(h.date), h.hour)),
            ),
        );
        tile(
            &mut cols[1],
            group(r.day.map_or(0, |d| d.1)),
            "busiest day",
            when(r.day.map(|(d, _)| day_label(d))),
        );
        tile(
            &mut cols[2],
            group(r.week.map_or(0, |w| w.1)),
            "busiest week",
            when(r.week.map(|(d, _)| format!("from {}", day_label(d)))),
        );
    });
    ui.add_space(12.0);
    let s = stats.streaks;
    ui.columns(3, |cols| {
        tile(
            &mut cols[0],
            days(s.current),
            "current streak",
            "active days in a row".into(),
        );
        tile(
            &mut cols[1],
            days(s.longest),
            "longest streak",
            s.longest_end
                .map_or_else(String::new, |d| format!("ended {}", short_date(d))),
        );
        let ratio = if stats.total.clicks == 0 {
            "–".into()
        } else {
            format!("{:.1}", stats.total.keys as f64 / stats.total.clicks as f64)
        };
        tile(&mut cols[2], ratio, "keys per click", "all time".into());
    });
}

fn keyboard(ui: &mut egui::Ui, k: &KeyView) {
    ui.columns(2, |cols| {
        card(&mut cols[0], |ui| {
            ui.set_min_height(PANEL_HEIGHT * 2.0);
            caption(ui, "BACKSPACE");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new(percent(k.insights.backspace, k.insights.total))
                        .size(24.0)
                        .color(BRAND_COTTONTAIL),
                );
                ui.label("of key presses");
            });
            weak(
                ui,
                format!(
                    "{} of {}, all time",
                    group(k.insights.backspace),
                    group(k.insights.total)
                ),
            );
            ui.add_space(20.0);
            caption(ui, "HANDS");
            hand_split(ui, k.insights.left, k.insights.right);
            ui.add_space(4.0);
            weak(
                ui,
                "By key position, touch-typing split. Space left out.".into(),
            );
        });
        card(&mut cols[1], |ui| {
            ui.set_min_height(PANEL_HEIGHT * 2.0);
            caption(ui, "TOP 10 KEYS");
            let rows: Vec<_> = k
                .names
                .iter()
                .cloned()
                .zip(k.insights.top.iter().map(|t| t.1))
                .collect();
            ranked_rows(ui, &rows, 84.0);
        });
    });
}
