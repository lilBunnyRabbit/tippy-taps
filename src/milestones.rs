use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::counts::{Bucket, group};
use crate::{settings, storage};

const STEPS: [u64; 7] = [1_000, 10_000, 50_000, 100_000, 250_000, 500_000, 1_000_000];
const EVERY: u64 = 1_000_000;
const QUEUE_MAX: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Milestone {
    pub count: &'static str,
    pub value: u64,
}

impl Milestone {
    fn key(&self) -> String {
        format!("{}:{}", self.count, self.value)
    }

    pub fn label(&self) -> String {
        let noun = match self.count {
            "lt_pulls" => "LT pulls",
            "rt_pulls" => "RT pulls",
            "total" => "inputs in total",
            other => other,
        };
        format!("{} {noun}!", group(self.value))
    }
}

pub fn reached(value: u64) -> impl Iterator<Item = u64> {
    STEPS
        .into_iter()
        .take_while(move |s| *s <= value)
        .chain((2..=value / EVERY).map(|n| n * EVERY))
}

fn counts(total: Bucket) -> [(&'static str, u64); 7] {
    [
        ("keys", total.keys),
        ("clicks", total.clicks),
        ("scrolls", total.scrolls),
        ("buttons", total.buttons),
        ("lt_pulls", total.lt_pulls),
        ("rt_pulls", total.rt_pulls),
        ("total", total.total()),
    ]
}

pub fn new_milestones(total: Bucket, celebrated: &BTreeSet<String>) -> Vec<Milestone> {
    counts(total)
        .into_iter()
        .flat_map(|(count, value)| reached(value).map(move |value| Milestone { count, value }))
        .filter(|m| !celebrated.contains(&m.key()))
        .collect()
}

/// Several thresholds of one count crossed at once (a long hidden stretch) get one show: the
/// highest.
pub fn to_celebrate(new: &[Milestone]) -> Vec<Milestone> {
    let mut top: Vec<Milestone> = Vec::new();
    for m in new {
        match top.iter_mut().find(|t| t.count == m.count) {
            Some(t) if t.value < m.value => *t = *m,
            Some(_) => {}
            None => top.push(*m),
        }
    }
    top
}

static QUEUE: Mutex<Vec<Milestone>> = Mutex::new(Vec::new());
static BASELINED: AtomicBool = AtomicBool::new(false);

pub fn take_queued() -> Vec<Milestone> {
    std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|p| p.into_inner()))
}

fn enqueue(queue: &mut Vec<Milestone>, shows: Vec<Milestone>) {
    queue.extend(shows);
    let excess = queue.len().saturating_sub(QUEUE_MAX);
    queue.drain(..excess);
}

/// Records newly reached milestones and queues fireworks for them when they are on. The first
/// check of a session only records: anything reached then was crossed before we were watching
/// (or while settings.json was missing). Returns true when fireworks are due. `played_now`: a
/// visible rabbit plays them, so they don't wait for the window; nothing is taken back out of
/// the queue, so shows queued earlier can't be lost.
pub fn check(total: Bucket, played_now: bool) -> bool {
    let new = settings::read(|s| new_milestones(total, &s.celebrated));
    if new.is_empty() {
        BASELINED.store(true, Ordering::Relaxed);
        return false;
    }
    settings::update(|s| s.celebrated.extend(new.iter().map(Milestone::key)));
    if !BASELINED.swap(true, Ordering::Relaxed) || !settings::fireworks() {
        return false;
    }
    if !played_now {
        enqueue(
            &mut QUEUE.lock().unwrap_or_else(|p| p.into_inner()),
            to_celebrate(&new),
        );
    }
    true
}

/// Totals are only complete once every shard has loaded; until then a check would celebrate
/// counts that merely reappeared.
pub fn check_storage(pending: Bucket, played_now: bool) -> bool {
    let total = {
        let storage = storage::get();
        if storage.saving_blocked() {
            return false;
        }
        let mut total = storage.total;
        total += pending;
        total
    };
    check(total, played_now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_drops_its_oldest_shows_past_the_cap() {
        let show = |value| Milestone {
            count: "keys",
            value,
        };
        let mut queue = vec![show(1), show(2)];
        enqueue(
            &mut queue,
            (1..=QUEUE_MAX as u64).map(|n| show(n * 10)).collect(),
        );
        assert_eq!(queue.len(), QUEUE_MAX);
        assert_eq!(queue[0], show(10));
        assert_eq!(queue.last(), Some(&show(QUEUE_MAX as u64 * 10)));
    }

    #[test]
    fn thresholds() {
        assert_eq!(reached(999).count(), 0);
        assert_eq!(reached(1_000).collect::<Vec<_>>(), [1_000]);
        assert_eq!(
            reached(260_000).collect::<Vec<_>>(),
            [1_000, 10_000, 50_000, 100_000, 250_000]
        );
        assert_eq!(
            reached(3_500_000).skip(6).collect::<Vec<_>>(),
            [1_000_000, 2_000_000, 3_000_000]
        );
    }

    #[test]
    fn crossing_fires_once() {
        let mut celebrated = BTreeSet::new();
        let keys = |keys| Bucket {
            keys,
            ..Default::default()
        };
        assert!(new_milestones(keys(999), &celebrated).is_empty());

        let new = new_milestones(keys(1_000), &celebrated);
        assert_eq!(
            new,
            [
                Milestone {
                    count: "keys",
                    value: 1_000
                },
                Milestone {
                    count: "total",
                    value: 1_000
                }
            ]
        );
        celebrated.extend(new.iter().map(Milestone::key));
        assert!(new_milestones(keys(9_999), &celebrated).is_empty());
        assert_eq!(new_milestones(keys(10_000), &celebrated).len(), 2);
    }

    #[test]
    fn total_counts_every_input() {
        let b = Bucket {
            keys: 400,
            clicks: 300,
            buttons: 200,
            rt_pulls: 100,
            lt_ms: 50_000,
            ..Default::default()
        };
        let new = new_milestones(b, &BTreeSet::new());
        assert_eq!(
            new,
            [Milestone {
                count: "total",
                value: 1_000
            }]
        );
    }

    #[test]
    fn a_jump_celebrates_only_the_highest_per_count() {
        let new = new_milestones(
            Bucket {
                keys: 120_000,
                clicks: 1_500,
                ..Default::default()
            },
            &BTreeSet::new(),
        );
        let shows = to_celebrate(&new);
        assert_eq!(
            shows,
            [
                Milestone {
                    count: "keys",
                    value: 100_000
                },
                Milestone {
                    count: "clicks",
                    value: 1_000
                },
                Milestone {
                    count: "total",
                    value: 100_000
                },
            ]
        );
        assert_eq!(shows[0].label(), "100,000 keys!");
        assert_eq!(shows[2].label(), "100,000 inputs in total!");
    }
}
