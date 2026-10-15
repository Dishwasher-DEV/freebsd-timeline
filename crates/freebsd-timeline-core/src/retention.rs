//! Decides which automatic snapshots to prune. Classic grandfather-father-son:
//! keep the newest snapshot of each of the last N hours, days, weeks, months
//! and years. A snapshot kept by any rule survives.
//!
//! Only `freebsd-timeline-auto-*` snapshots are ever candidates. Manual, pre-upgrade and
//! foreign snapshots are never touched.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::config::Keep;
use crate::model::Snapshot;
use crate::time;

/// Never prune anything younger than this, so a snapshot taken a moment ago
/// can't race with the pruner.
const GRACE_SECS: i64 = 600;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrunePlan {
    pub keep: Vec<Snapshot>,
    pub destroy: Vec<Snapshot>,
}

pub fn plan(snaps: &[Snapshot], keep: &Keep, now: i64) -> PrunePlan {
    let mut by_dataset: BTreeMap<&str, Vec<&Snapshot>> = BTreeMap::new();
    for s in snaps.iter().filter(|s| s.is_auto()) {
        by_dataset.entry(&s.dataset).or_default().push(s);
    }

    type BucketKey = fn(i64) -> i64;
    let rules: [(u32, BucketKey); 5] = [
        (keep.hourly, time::hour_key),
        (keep.daily, time::day_key),
        (keep.weekly, time::week_key),
        (keep.monthly, time::month_key),
        (keep.yearly, time::year_key),
    ];

    let mut result = PrunePlan::default();
    for (_, mut list) in by_dataset {
        list.sort_by_key(|s| std::cmp::Reverse(s.created)); // newest first
        let mut kept: HashSet<usize> = HashSet::new();
        kept.insert(0); // always keep the newest

        for (count, key) in rules {
            let mut seen = HashSet::new();
            for (i, s) in list.iter().enumerate() {
                if seen.len() >= count as usize {
                    break;
                }
                if seen.insert(key(s.created)) {
                    kept.insert(i);
                }
            }
        }

        for (i, s) in list.into_iter().enumerate() {
            if kept.contains(&i) || now - s.created < GRACE_SECS {
                result.keep.push(s.clone());
            } else {
                result.destroy.push(s.clone());
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::managed_name;

    fn snap(ds: &str, label: &str, t: i64) -> Snapshot {
        Snapshot {
            dataset: ds.into(),
            name: managed_name(label, t),
            created: t,
            used: 0,
            referenced: 0,
        }
    }

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn hourly_for_three_days() {
        let snaps: Vec<_> = (0..72)
            .map(|h| snap("zroot/home", "auto", NOW - h * 3600))
            .collect();
        let keep = Keep {
            hourly: 24,
            daily: 7,
            weekly: 0,
            monthly: 0,
            yearly: 0,
        };
        let p = plan(&snaps, &keep, NOW);
        // 24 hourly + newest of the 2 older days (the third day overlaps the hourly window)
        assert!(
            p.keep.len() >= 24 && p.keep.len() <= 27,
            "kept {}",
            p.keep.len()
        );
        assert_eq!(p.keep.len() + p.destroy.len(), 72);
    }

    #[test]
    fn never_touches_manual_or_foreign() {
        let snaps = vec![
            snap("zroot/home", "manual", NOW - 90 * 86400),
            snap("zroot/home", "pre-upgrade", NOW - 90 * 86400),
            Snapshot {
                dataset: "zroot/home".into(),
                name: "mine".into(),
                created: 0,
                used: 0,
                referenced: 0,
            },
        ];
        let keep = Keep {
            hourly: 0,
            daily: 0,
            weekly: 0,
            monthly: 0,
            yearly: 0,
        };
        let p = plan(&snaps, &keep, NOW);
        assert!(p.destroy.is_empty());
    }

    #[test]
    fn always_keeps_newest_and_grace() {
        let snaps = vec![
            snap("zroot/home", "auto", NOW - 60),
            snap("zroot/home", "auto", NOW - 120),
            snap("zroot/home", "auto", NOW - 100 * 86400),
        ];
        let keep = Keep {
            hourly: 0,
            daily: 0,
            weekly: 0,
            monthly: 0,
            yearly: 0,
        };
        let p = plan(&snaps, &keep, NOW);
        assert_eq!(p.keep.len(), 2);
        assert_eq!(p.destroy.len(), 1);
        assert_eq!(p.destroy[0].created, NOW - 100 * 86400);
    }

    #[test]
    fn datasets_are_independent() {
        let snaps = vec![
            snap("a", "auto", NOW - 50 * 86400),
            snap("b", "auto", NOW - 60 * 86400),
        ];
        let p = plan(&snaps, &Keep::default(), NOW);
        assert_eq!(p.keep.len(), 2);
    }
}
