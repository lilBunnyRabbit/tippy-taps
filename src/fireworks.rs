use std::collections::VecDeque;

use eframe::egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Vec2, pos2, vec2};

const GRAVITY: f32 = 140.0;
const DRAG: f32 = 0.9;
const BURSTS: u32 = 6;
const BURST_GAP: f32 = 0.35;
const SPARKS: usize = 36;
const SPARK_LIFE: f32 = 1.4;
const BANNER_SECS: f32 = 3.5;
const MAX_DT: f32 = 0.05;

struct Spark {
    pos: Pos2,
    vel: Vec2,
    age: f32,
    life: f32,
    color: Color32,
}

/// Placeholder show until the rabbit art lands: bursts of sparks plus a banner.
pub struct Fireworks {
    sparks: Vec<Spark>,
    bursts_left: u32,
    next_burst: f32,
    /// Shown one after another; the front one ages.
    banners: VecDeque<String>,
    banner_age: f32,
    rng: u64,
    palette: [Color32; 4],
}

impl Fireworks {
    pub fn new(palette: [Color32; 4], seed: u64) -> Self {
        Self {
            sparks: Vec::new(),
            bursts_left: 0,
            next_burst: 0.0,
            banners: VecDeque::new(),
            banner_age: 0.0,
            rng: seed | 1,
            palette,
        }
    }

    pub fn active(&self) -> bool {
        self.bursts_left > 0 || !self.sparks.is_empty() || !self.banners.is_empty()
    }

    pub fn launch(&mut self, label: String) {
        self.bursts_left += BURSTS;
        self.banners.push_back(label);
    }

    fn random(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }

    pub fn step(&mut self, dt: f32, area: Rect) {
        let dt = dt.clamp(0.0, MAX_DT);
        if self.bursts_left > 0 {
            self.next_burst -= dt;
            if self.next_burst <= 0.0 {
                self.bursts_left -= 1;
                self.next_burst = BURST_GAP;
                self.burst(area);
            }
        }
        for s in &mut self.sparks {
            s.age += dt;
            s.vel.y += GRAVITY * dt;
            s.vel *= DRAG.powf(dt);
            s.pos += s.vel * dt;
        }
        self.sparks.retain(|s| s.age < s.life);
        if !self.banners.is_empty() {
            self.banner_age += dt;
            if self.banner_age > BANNER_SECS {
                self.banners.pop_front();
                self.banner_age = 0.0;
            }
        }
    }

    fn burst(&mut self, area: Rect) {
        let center = pos2(
            area.left() + area.width() * (0.2 + 0.6 * self.random()),
            area.top() + area.height() * (0.15 + 0.35 * self.random()),
        );
        let color = self.palette[(self.random() * self.palette.len() as f32) as usize % 4];
        for i in 0..SPARKS {
            let angle = std::f32::consts::TAU * (i as f32 + self.random() * 0.5) / SPARKS as f32;
            let speed = 70.0 + 110.0 * self.random();
            let life = SPARK_LIFE * (0.7 + 0.3 * self.random());
            self.sparks.push(Spark {
                pos: center,
                vel: Vec2::angled(angle) * speed,
                age: 0.0,
                life,
                color,
            });
        }
    }

    pub fn paint(&self, painter: &Painter, area: Rect, banner_bg: Color32) {
        for s in &self.sparks {
            let fade = 1.0 - s.age / s.life;
            painter.circle_filled(s.pos, 1.0 + 1.6 * fade, s.color.gamma_multiply(fade));
        }
        if let Some(text) = self.banners.front() {
            let age = self.banner_age;
            let alpha = (BANNER_SECS - age).clamp(0.0, 0.5) * 2.0;
            let galley = painter.layout_no_wrap(
                text.clone(),
                FontId::proportional(22.0),
                self.palette[0].gamma_multiply(alpha),
            );
            let center = pos2(area.center().x, area.top() + area.height() * 0.4);
            let rect = Align2::CENTER_CENTER
                .anchor_size(center, galley.size())
                .expand2(vec2(14.0, 8.0));
            painter.rect_filled(rect, 8.0, banner_bg.gamma_multiply(alpha * 0.92));
            painter.galley(rect.min + vec2(14.0, 8.0), galley, Color32::WHITE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_show_runs_then_ends() {
        let mut fw = Fireworks::new([Color32::WHITE; 4], 42);
        assert!(!fw.active());
        fw.launch("1,000 keys!".into());
        let area = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 600.0));
        let mut peak = 0;
        for _ in 0..400 {
            fw.step(1.0 / 60.0, area);
            peak = peak.max(fw.sparks.len());
            assert!(fw.sparks.iter().all(|s| s.pos.x.is_finite()));
        }
        assert!(peak > SPARKS);
        assert!(!fw.active(), "everything has faded after ~6.7 s");
    }

    #[test]
    fn banners_play_in_turn() {
        let mut fw = Fireworks::new([Color32::WHITE; 4], 7);
        fw.launch("1,000 keys!".into());
        fw.launch("1,000 inputs in total!".into());
        let area = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 600.0));
        for _ in 0..60 {
            fw.step(1.0 / 60.0, area);
        }
        assert_eq!(fw.banners.front().map(String::as_str), Some("1,000 keys!"));
        for _ in 0..180 {
            fw.step(1.0 / 60.0, area);
        }
        assert_eq!(
            fw.banners.front().map(String::as_str),
            Some("1,000 inputs in total!")
        );
    }
}
