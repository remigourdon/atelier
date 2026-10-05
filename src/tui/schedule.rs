//! When to refresh and list the feeds, and what is loading: the jobs to start, with no I/O.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use super::app::{Feed, Job, Source};

/// Seconds between refreshes: a fast one once idle, a full one regardless.
pub const FAST_REFRESH: u32 = 10;
pub const FULL_REFRESH: u32 = 300;

/// Whether a feed is listed at its next turn, and whether from the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    No,
    Cached,
    /// Past the cache, as `R` asks.
    Fresh,
}

pub struct Schedule {
    /// Jobs in flight by source.
    loading: BTreeMap<Source, usize>,
    /// Worktrees with a pull in flight, which show a spinner.
    pulling: HashSet<PathBuf>,
    /// Seconds since the last input, the last refresh and the last full refresh.
    idle: u32,
    since_refresh: u32,
    since_full: u32,
    due: BTreeMap<Feed, Due>,
    /// What each feed lists, as the last snapshot named them.
    keys: BTreeMap<Feed, Vec<String>>,
    /// A refresh asked for while one runs, and whether it is full.
    pending: Option<bool>,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            loading: BTreeMap::new(),
            pulling: HashSet::new(),
            idle: 0,
            since_refresh: 0,
            since_full: 0,
            // At startup, so the panels fill.
            due: Feed::ALL
                .into_iter()
                .map(|feed| (feed, Due::Cached))
                .collect(),
            keys: BTreeMap::new(),
            pending: None,
        }
    }
}

impl Schedule {
    /// Once a second: a full refresh every five minutes, a fast one when idle.
    pub fn tick(&mut self) -> Vec<Job> {
        self.idle += 1;
        self.since_refresh += 1;
        self.since_full += 1;
        if self.is_loading(Source::Wt) {
            Vec::new()
        } else if self.since_full >= FULL_REFRESH {
            self.refresh(false)
        } else if self.idle >= FAST_REFRESH && self.since_refresh >= FAST_REFRESH {
            self.request(false)
        } else {
            Vec::new()
        }
    }

    pub fn input(&mut self) {
        self.idle = 0;
    }

    /// A full refresh, past the cache when `force`. Every feed is due: issues start at once,
    /// reviews wait for the snapshot that names their hosts.
    pub fn refresh(&mut self, force: bool) -> Vec<Job> {
        for due in self.due.values_mut() {
            if force {
                *due = Due::Fresh;
            } else if *due == Due::No {
                *due = Due::Cached;
            }
        }
        let mut jobs = self.request(true);
        jobs.extend(self.fetch(|feed| matches!(feed, Feed::Issues(_))));
        jobs
    }

    /// A job changed items, workspaces, repos or tabs: a fast refresh shows it.
    pub fn changed(&mut self) -> Vec<Job> {
        self.request(false)
    }

    /// A snapshot named each feed's keys: lists the due feeds.
    pub fn loaded(&mut self, keys: impl IntoIterator<Item = (Feed, Vec<String>)>) -> Vec<Job> {
        self.keys = keys.into_iter().collect();
        self.fetch(|_| true)
    }

    pub fn started(&mut self, job: &Job) {
        *self.loading.entry(job.source()).or_default() += 1;
        match job {
            Job::Pull(paths) => self.pulling.extend(paths.iter().cloned()),
            Job::Refresh { full } => {
                self.since_refresh = 0;
                if *full {
                    self.since_full = 0;
                }
            }
            _ => {}
        }
    }

    /// A job of `source` finished; once no refresh runs, the one pending starts.
    pub fn finished(&mut self, source: Source) -> Vec<Job> {
        if let Some(count) = self.loading.get_mut(&source) {
            *count -= 1;
            if *count == 0 {
                self.loading.remove(&source);
            }
        }
        if source == Source::Wt
            && !self.is_loading(Source::Wt)
            && let Some(full) = self.pending.take()
        {
            return vec![Job::Refresh { full }];
        }
        Vec::new()
    }

    pub fn pulled(&mut self, paths: &[PathBuf]) {
        for path in paths {
            self.pulling.remove(path);
        }
    }

    pub fn is_loading(&self, source: Source) -> bool {
        self.loading.contains_key(&source)
    }

    /// What is loading, in the hint bar's order.
    pub fn loading(&self) -> impl Iterator<Item = Source> + '_ {
        self.loading.keys().copied()
    }

    pub fn is_pulling(&self, path: &PathBuf) -> bool {
        self.pulling.contains(path)
    }

    /// Whether a spinner turns.
    pub fn animating(&self) -> bool {
        !self.pulling.is_empty()
    }

    /// Starts a refresh, or marks one pending while another runs.
    fn request(&mut self, full: bool) -> Vec<Job> {
        if self.is_loading(Source::Wt) {
            self.pending = Some(self.pending.unwrap_or(false) || full);
            Vec::new()
        } else {
            vec![Job::Refresh { full }]
        }
    }

    /// Lists the due feeds that are `ready`. A feed still listing, or with nothing to list
    /// yet (before the first snapshot), keeps its turn, so `R` is not lost.
    fn fetch(&mut self, ready: impl Fn(Feed) -> bool) -> Vec<Job> {
        let mut jobs = Vec::new();
        for feed in Feed::ALL {
            let due = self.due[&feed];
            if due == Due::No || !ready(feed) || self.is_loading(Source::Feed(feed)) {
                continue;
            }
            if let Some(keys) = self.keys.get(&feed).filter(|keys| !keys.is_empty()) {
                self.due.insert(feed, Due::No);
                jobs.push(Job::Fetch {
                    feed,
                    keys: keys.clone(),
                    force: due == Due::Fresh,
                });
            }
        }
        jobs
    }

    /// Finishes every job in flight, dropping what that would start.
    #[cfg(test)]
    pub fn finish_all(&mut self) {
        while let Some(&source) = self.loading.keys().next() {
            self.finished(source);
        }
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::Tracker;
    use crate::reviews::Provider;

    const GH: Feed = Feed::Reviews(Provider::GitHub);
    const LAB: Feed = Feed::Reviews(Provider::GitLab);
    const ISSUES: Feed = Feed::Issues(Tracker::GitHub);

    /// Starts the jobs, as `update` does.
    fn start(schedule: &mut Schedule, jobs: Vec<Job>) -> Vec<Job> {
        jobs.iter().for_each(|job| schedule.started(job));
        jobs
    }

    fn keys() -> Vec<(Feed, Vec<String>)> {
        vec![
            (GH, vec!["github.com".into()]),
            (LAB, vec!["gitlab.com".into()]),
            (ISSUES, vec!["o/a".into()]),
        ]
    }

    /// Each fetch started, by feed and whether past the cache.
    fn fetches(jobs: &[Job]) -> Vec<(Feed, bool)> {
        (jobs.iter())
            .filter_map(|job| match job {
                Job::Fetch { feed, force, .. } => Some((*feed, *force)),
                _ => None,
            })
            .collect()
    }

    /// Past startup's listing, with nothing in flight.
    fn settled() -> Schedule {
        let mut schedule = Schedule::default();
        let jobs = schedule.loaded(keys());
        start(&mut schedule, jobs);
        schedule.finish_all();
        schedule
    }

    fn ticks(schedule: &mut Schedule, seconds: u32) -> Vec<Job> {
        let mut jobs = Vec::new();
        for _ in 0..seconds {
            let started = schedule.tick();
            jobs.extend(start(schedule, started));
        }
        jobs
    }

    #[test]
    fn r_before_the_first_snapshot_lists_every_feed_once_it_arrives() {
        let mut schedule = Schedule::default();
        let jobs = schedule.refresh(true);
        assert!(fetches(&jobs).is_empty(), "nothing to list yet");
        start(&mut schedule, jobs);
        schedule.finish_all();
        assert_eq!(
            fetches(&schedule.loaded(keys())),
            [(GH, true), (LAB, true), (ISSUES, true)]
        );
    }

    #[test]
    fn startup_lists_every_feed_from_the_cache() {
        let mut schedule = Schedule::default();
        assert_eq!(
            fetches(&schedule.loaded(keys())),
            [(GH, false), (LAB, false), (ISSUES, false)]
        );
        assert!(schedule.loaded(keys()).is_empty(), "once");
    }

    #[test]
    fn a_fast_refresh_every_ten_seconds_once_idle() {
        let mut schedule = settled();
        assert!(ticks(&mut schedule, FAST_REFRESH - 1).is_empty());
        schedule.input();
        assert!(ticks(&mut schedule, FAST_REFRESH - 1).is_empty(), "input");
        assert_eq!(ticks(&mut schedule, 1), [Job::Refresh { full: false }]);
        assert!(
            ticks(&mut schedule, FAST_REFRESH).is_empty(),
            "one in flight"
        );
        schedule.finished(Source::Wt);
        assert_eq!(ticks(&mut schedule, 1), [Job::Refresh { full: false }]);
    }

    #[test]
    fn a_full_refresh_every_five_minutes_lists_the_feeds() {
        let mut schedule = settled();
        let mut jobs = Vec::new();
        for _ in 0..FULL_REFRESH {
            schedule.input();
            jobs.extend(ticks(&mut schedule, 1));
            schedule.finish_all();
        }
        assert_eq!(jobs[0], Job::Refresh { full: true });
        assert_eq!(fetches(&jobs), [(ISSUES, false)], "issues start at once");
        assert_eq!(
            fetches(&schedule.loaded(keys())),
            [(GH, false), (LAB, false)],
            "reviews wait for the snapshot"
        );
    }

    #[test]
    fn r_lists_every_feed_past_the_cache() {
        let mut schedule = settled();
        let jobs = schedule.refresh(true);
        let jobs = start(&mut schedule, jobs);
        assert_eq!(jobs[0], Job::Refresh { full: true });
        assert_eq!(fetches(&jobs), [(ISSUES, true)]);
        assert_eq!(fetches(&schedule.loaded(keys())), [(GH, true), (LAB, true)]);
    }

    #[test]
    fn a_feed_still_listing_keeps_its_own_turn() {
        let mut schedule = settled();
        start(
            &mut schedule,
            vec![Job::Fetch {
                feed: GH,
                keys: Vec::new(),
                force: false,
            }],
        );
        schedule.refresh(true);
        assert_eq!(
            fetches(&schedule.loaded(keys())),
            [(LAB, true)],
            "the idle feed lists at once"
        );
        assert!(
            schedule.loaded(keys()).is_empty(),
            "GitHub is still listing"
        );
        schedule.finished(Source::Feed(GH));
        assert_eq!(
            fetches(&schedule.loaded(keys())),
            [(GH, true)],
            "then R lists it, and only it"
        );
    }

    #[test]
    fn a_feed_without_keys_is_not_listed() {
        let mut schedule = Schedule::default();
        assert!(fetches(&schedule.loaded([(GH, Vec::new())])).is_empty());
    }

    #[test]
    fn a_refresh_asked_while_one_runs_waits_for_it() {
        let mut schedule = settled();
        let jobs = schedule.changed();
        start(&mut schedule, jobs);
        assert!(schedule.changed().is_empty());
        assert_eq!(fetches(&schedule.refresh(false)), [(ISSUES, false)]);
        assert!(schedule.changed().is_empty(), "one pending, not three");
        assert_eq!(
            schedule.finished(Source::Wt),
            [Job::Refresh { full: true }],
            "full, as one of them asked"
        );
        assert!(schedule.finished(Source::Wt).is_empty());
    }

    #[test]
    fn pulls_spin_until_they_finish() {
        let mut schedule = settled();
        let paths = vec![PathBuf::from("/src/api")];
        schedule.started(&Job::Pull(paths.clone()));
        assert!(schedule.animating() && schedule.is_pulling(&paths[0]));
        assert!(schedule.is_loading(Source::Run));
        schedule.finished(Source::Run);
        schedule.pulled(&paths);
        assert!(!schedule.animating());
    }
}
