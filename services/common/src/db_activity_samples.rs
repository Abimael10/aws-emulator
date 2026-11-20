// SPDX-License-Identifier: CC0-1.0

//! The DB SESSION ACTIVITY a database engine this emulator runs has had: one sample per
//! unit of work a session did, written by the database DATA plane and read by the services
//! that report on database load.
//!
//! A monitoring service reports WHICH sessions were busy on a DB resource over a window,
//! grouped by what they were waiting on, which statement they ran, which user opened them
//! and which host they came from. It measures none of that itself: the engine is what runs
//! the sessions, so the engine's own data plane is the only place those facts exist, and a
//! window in the past can only be reported if the activity was RECORDED as it happened —
//! a reader asked afterwards can see nothing but the present instant.
//!
//! This is that record: ONE store in the shared engine, written by the engine wires as each
//! statement finishes and read by the monitoring services. Nothing here names a wire shape
//! of any model: a sample holds the facts of one unit of session activity — when it was
//! observed, how long the session was active for it, what the session was waiting on while
//! it was, and who was running what — and a reader projects those onto the dimensions ITS
//! model declares.
//!
//! The store is a record of what this emulator RAN, so it is partial by construction: a
//! resource whose engine has served no statement holds no samples, which is the honest
//! report that nothing was active on it rather than evidence about anything else. It is also
//! BOUNDED twice over — [`MAX_SAMPLES_PER_DATA_SOURCE`] per resource and
//! [`MAX_SAMPLED_DATA_SOURCES`] resources — so a workload running statements in a loop
//! cannot grow it without end; the OLDEST sample (and the oldest resource) is dropped first,
//! which keeps the activity a window just observed reportable.
//!
//! LOCK ORDER: this store is taken LAST and released before anything else is locked. The
//! data plane records a sample while it holds no other store, and a monitoring service reads
//! it without holding its own.

use std::collections::{HashMap, VecDeque};

/// How many samples one data source keeps.
///
/// An EMULATOR ceiling, not an AWS quota: the store lives in memory for the lifetime of the
/// process and a session can run statements in a loop, so without a bound the activity of
/// one busy resource would grow it without end. Past it the OLDEST sample is dropped —
/// never a newer one — so the window a monitoring service is reporting on now is the window
/// whose samples are kept.
pub const MAX_SAMPLES_PER_DATA_SOURCE: usize = 1_024;

/// How many data sources hold samples at once, bounded for the same reason and with the
/// same rule: the resource whose engine has been quiet longest is the one dropped.
pub const MAX_SAMPLED_DATA_SOURCES: usize = 256;

/// The longest statement text a sample carries.
///
/// The monitoring APIs that report a statement cap what they return at 500 bytes
/// (`DescribeDimensionKeys`: "Each response element returns a maximum of 500 bytes. For
/// larger elements, such as SQL statements, only the first 500 bytes are returned"), so a
/// sample holds no more of it than any reader may report.
pub const MAX_STATEMENT_BYTES: usize = 500;

/// What a session was waiting on while it was active, as the engine names it.
///
/// The two engines name their own events (`Lock:transactionid` on PostgreSQL,
/// `synch/cond/mysys/my_thread_var::suspend` on MySQL), and a session that was waiting for
/// nothing at all was on CPU, which both report under the one event name `CPU`. The TYPE is
/// the class the engine groups the event under, which is what a reader that groups by event
/// type reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitEvent {
    /// The event's own name, as the engine publishes it.
    pub name: String,
    /// The class the engine groups it under.
    pub event_type: String,
}

/// The event name every engine reports for a session that was running rather than waiting.
pub const WAIT_EVENT_CPU: &str = "CPU";

impl WaitEvent {
    /// The event a session that was waiting for nothing was "on": CPU, whose own class is
    /// itself.
    #[must_use]
    pub fn on_cpu() -> Self {
        Self {
            name: WAIT_EVENT_CPU.to_string(),
            event_type: WAIT_EVENT_CPU.to_string(),
        }
    }
}

/// ONE sample of session activity on a data source: a session was active for
/// `active_seconds`, waiting on `wait_event`, at `at_epoch_seconds`.
#[derive(Debug, Clone)]
pub struct ActivitySample {
    /// When the activity was observed, in epoch seconds — the instant a reader's window is
    /// matched against.
    pub at_epoch_seconds: f64,
    /// How long the session was active in this state, in seconds. The weight of the sample:
    /// a reader's average-active-sessions figure over a window is the active seconds the
    /// window holds divided by its own length.
    pub active_seconds: f64,
    /// What the session was waiting on while it was active.
    pub wait_event: WaitEvent,
    /// The engine's own id of the session (a PostgreSQL backend pid, a MySQL connection id).
    pub session_id: u32,
    /// The database user the session authenticated as.
    pub user: String,
    /// The address the session connected from.
    pub host: String,
    /// The database the session was connected to.
    pub database: String,
    /// The `application_name` the session declared, empty where it declared none.
    pub application: String,
    /// The statement the session was running, truncated to [`MAX_STATEMENT_BYTES`].
    pub statement: String,
    /// The same statement with its literal values replaced, which is the form a reader that
    /// groups statements by their digest reports.
    pub statement_digest: String,
}

/// The activity samples of every data source whose engine this emulator runs.
///
/// Registered in the shared engine under THIS type, so the data plane that runs the
/// sessions and the service that reports on them use one store rather than each keeping a
/// copy.
#[derive(Debug, Default)]
pub struct DbActivitySamples {
    /// The samples, by the IMMUTABLE resource id of the data source they were observed on
    /// (`DbiResourceId` for a DB instance, `DbClusterResourceId` for a cluster) — the same
    /// id the data-source directory keys a resource by ([`crate::db_data_sources`]).
    samples: HashMap<String, VecDeque<ActivitySample>>,
    /// Which resource was recorded into least recently, so the ceiling drops that one first.
    order: VecDeque<String>,
}

impl DbActivitySamples {
    /// Record one sample of activity on the data source `resource_id`.
    ///
    /// A sample weighing nothing is not activity, so it is not recorded: a statement that
    /// took no measurable time leaves the resource as idle as it found it.
    pub fn record(&mut self, resource_id: &str, sample: ActivitySample) {
        if sample.active_seconds <= 0.0 || !sample.active_seconds.is_finite() {
            return;
        }
        let samples = self.samples.entry(resource_id.to_string()).or_default();
        samples.push_back(sample);
        while samples.len() > MAX_SAMPLES_PER_DATA_SOURCE {
            samples.pop_front();
        }
        self.order.retain(|recorded| recorded != resource_id);
        self.order.push_back(resource_id.to_string());
        while self.order.len() > MAX_SAMPLED_DATA_SOURCES {
            if let Some(oldest) = self.order.pop_front() {
                self.samples.remove(&oldest);
            }
        }
    }

    /// The samples of `resource_id` observed in the window `[start, end)`, in the order they
    /// were observed.
    ///
    /// The start is inclusive and the end exclusive, the reading the monitoring APIs
    /// document for their own windows ("data points equal to or greater than StartTime",
    /// "data points less than (but not equal to) EndTime").
    #[must_use]
    pub fn in_window(&self, resource_id: &str, start: f64, end: f64) -> Vec<&ActivitySample> {
        self.samples
            .get(resource_id)
            .into_iter()
            .flatten()
            .filter(|sample| sample.at_epoch_seconds >= start && sample.at_epoch_seconds < end)
            .collect()
    }

    /// Drop every sample of `resource_id`: the resource it names is gone, so the activity of
    /// its engine is not something this emulator still reports.
    pub fn forget(&mut self, resource_id: &str) {
        self.samples.remove(resource_id);
        self.order.retain(|recorded| recorded != resource_id);
    }

    /// How many data sources hold samples.
    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Whether no data source holds a sample at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sample of `active_seconds` at `at`, with everything else fixed.
    fn sample(at: f64, active_seconds: f64) -> ActivitySample {
        ActivitySample {
            at_epoch_seconds: at,
            active_seconds,
            wait_event: WaitEvent::on_cpu(),
            session_id: 7,
            user: "appuser".to_string(),
            host: "127.0.0.1".to_string(),
            database: "appdb".to_string(),
            application: String::new(),
            statement: "SELECT 1".to_string(),
            statement_digest: "SELECT ?".to_string(),
        }
    }

    #[test]
    fn a_window_holds_the_samples_observed_inside_it() {
        let mut store = DbActivitySamples::default();
        // Each row is a sample's instant and whether the window [100, 200) holds it: the
        // start is inclusive, the end exclusive.
        let rows: &[(&str, f64, bool)] = &[
            ("before the window", 99.5, false),
            ("on the start bound", 100.0, true),
            ("inside", 150.25, true),
            ("on the end bound", 200.0, false),
            ("after the window", 200.5, false),
        ];
        for (_, at, _) in rows {
            store.record("db-SAMPLED0000000001", sample(*at, 0.5));
        }
        let held = store.in_window("db-SAMPLED0000000001", 100.0, 200.0);
        for (case, at, inside) in rows {
            let found = held
                .iter()
                .any(|sample| sample.at_epoch_seconds == *at);
            assert_eq!(
                found, *inside,
                "case {case}: expected the window to hold it = {inside}, got {found}"
            );
        }
        assert!(
            store
                .in_window("db-NEVERSAMPLED00001", 0.0, f64::MAX)
                .is_empty(),
            "case a resource nothing ran on: expected no samples, got some"
        );
    }

    #[test]
    fn a_sample_that_weighs_nothing_is_not_activity() {
        let mut store = DbActivitySamples::default();
        for (case, active_seconds) in [("zero", 0.0f64), ("negative", -1.0)] {
            store.record("db-IDLE000000000001", sample(10.0, active_seconds));
            assert!(
                store.is_empty(),
                "case {case} active seconds: expected nothing recorded, got {} source(s)",
                store.len()
            );
        }
        store.record("db-IDLE000000000001", sample(10.0, 0.001));
        assert_eq!(
            store.in_window("db-IDLE000000000001", 0.0, 20.0).len(),
            1,
            "case a measurable sample: expected it recorded"
        );
    }

    #[test]
    fn the_ceilings_drop_the_oldest_sample_and_the_quietest_resource() {
        let mut store = DbActivitySamples::default();
        for index in 0..=MAX_SAMPLES_PER_DATA_SOURCE {
            store.record("db-BUSY000000000001", sample(index as f64, 0.5));
        }
        let held = store.in_window("db-BUSY000000000001", 0.0, f64::MAX);
        assert_eq!(
            held.len(),
            MAX_SAMPLES_PER_DATA_SOURCE,
            "case one sample past the ceiling: expected the ceiling held, got {}",
            held.len()
        );
        assert!(
            held.iter().all(|sample| sample.at_epoch_seconds > 0.0),
            "case one sample past the ceiling: expected the oldest sample dropped"
        );

        for index in 0..=MAX_SAMPLED_DATA_SOURCES {
            store.record(&format!("db-MANY{index:015}"), sample(1.0, 0.5));
        }
        assert_eq!(
            store.len(),
            MAX_SAMPLED_DATA_SOURCES,
            "case one resource past the ceiling: expected the ceiling held, got {}",
            store.len()
        );
        assert!(
            store.in_window("db-MANY000000000000000", 0.0, f64::MAX).is_empty(),
            "case one resource past the ceiling: expected the quietest resource dropped"
        );
    }

    #[test]
    fn forgetting_a_resource_drops_its_samples_only() {
        let mut store = DbActivitySamples::default();
        store.record("db-KEPT000000000001", sample(1.0, 0.5));
        store.record("db-DROPPED00000001", sample(1.0, 0.5));
        store.forget("db-DROPPED00000001");
        assert!(
            store.in_window("db-DROPPED00000001", 0.0, f64::MAX).is_empty()
                && !store.in_window("db-KEPT000000000001", 0.0, f64::MAX).is_empty(),
            "case one resource forgotten: expected only its samples dropped, got {} source(s)",
            store.len()
        );
        // Forgetting a resource that holds none changes nothing, so a repeated delete is
        // harmless.
        store.forget("db-DROPPED00000001");
        store.forget("db-NEVERSAMPLED00001");
        assert_eq!(
            store.len(),
            1,
            "case forgetting an absent resource: expected the remaining samples untouched, got {}",
            store.len()
        );
    }
}
