// SPDX-License-Identifier: CC0-1.0

//! The DB SESSION ACTIVITY Performance Insights reports on, and the dimensions it groups
//! that activity by.
//!
//! Performance Insights is a REPORTING service: the load it reports is the load the DB
//! engine actually had, which the service that runs the engine records as it happens
//! ([`::service_common::db_activity_samples`]). Nothing here measures or invents activity —
//! these are the readings of what the engines recorded:
//!
//! * [`sampled_activity`], the samples of one data source inside a window;
//! * [`average_active_sessions`], the figure `db.load.avg` and `db.sampledload.avg` report
//!   for a span — the active session seconds the span holds divided by its own length,
//!   which is what "a scaled representation of the number of active sessions for the
//!   database engine" is;
//! * [`dimension_value`] and [`group_dimensions`], the projection of one sample onto the
//!   dimensions of a dimension GROUP, which is what `DescribeDimensionKeys` groups by,
//!   partitions by and filters on.
//!
//! A data source whose engine recorded nothing has no load, and a window holding no sample
//! reports no dimension key: the honest report of an idle resource, not an invented top-N.

use std::collections::BTreeMap;

use ::service_common::db_activity_samples::{ActivitySample, DbActivitySamples};

/// The SHARED activity store in the shared engine: call it with a handler's `&*self.engine`.
///
/// The samples are written by the database data plane that RAN the sessions, so they live in
/// one store under one shared type rather than in a copy of this crate's own — the engine
/// keys state by TYPE, and a copy here would be invisible to the service that records.
///
/// LOCK ORDER: taken and released before [`super::service_state`], as every other shared
/// store this crate reads is.
pub fn activity_store<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<DbActivitySamples>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<DbActivitySamples>)
}

/// The activity samples of the data source `identifier` observed in the window
/// `[start, end)`, in the order the engine observed them.
///
/// EMPTY for a data source whose owning service records Performance Insights as turned OFF:
/// the feature is what collects the activity, so a resource it is off for has none collected
/// for it, whatever its engine did — the same monitoring configuration
/// `GetResourceMetadata` projects its features from ([`super::observed_features`]).
pub fn sampled_activity<Eng: Send + Sync + 'static>(
    engine: &Eng,
    identifier: &str,
    start: f64,
    end: f64,
) -> Vec<ActivitySample> {
    if !collects_activity(engine, identifier) {
        return Vec::new();
    }
    let Some(store) = activity_store(engine) else {
        return Vec::new();
    };
    let guard = store.lock();
    guard
        .in_window(identifier, start, end)
        .into_iter()
        .cloned()
        .collect()
}

/// Whether Performance Insights collects activity for the data source `identifier` at all.
///
/// The owning service's record decides it where there is one: a resource it persisted
/// `EnablePerformanceInsights` as off for collects nothing. A data source the directory holds
/// no record of is UNDECIDED — this emulator only knows what it minted — so what its engine
/// recorded is reported rather than suppressed on a missing record.
fn collects_activity<Eng: Send + Sync + 'static>(engine: &Eng, identifier: &str) -> bool {
    let Some(directory) = super::data_source_directory(engine) else {
        return true;
    };
    let guard = directory.lock();
    guard.get(identifier).is_none_or(|record| {
        record
            .monitoring
            .as_ref()
            .is_none_or(|monitoring| monitoring.performance_insights_enabled)
    })
}

/// The AVERAGE ACTIVE SESSIONS the samples hold over a span of `span_seconds`: the active
/// session time they account for, divided by the span.
///
/// `Metric` documents `db.load.avg` as "a scaled representation of the number of active
/// sessions for the database engine" and `db.sampledload.avg` as "the raw number of active
/// sessions"; both are the same figure here, which the member itself allows ("if the number
/// of active sessions is less than an internal Performance Insights threshold, db.load.avg
/// and db.sampledload.avg are the same value") — this emulator samples every unit of
/// activity rather than a fraction of it, so there is nothing to scale.
///
/// Zero for a span that holds no sample, and for a span of no length: no activity is no load.
#[must_use]
pub fn average_active_sessions<'a>(
    samples: impl IntoIterator<Item = &'a ActivitySample>,
    span_seconds: f64,
) -> f64 {
    if span_seconds <= 0.0 {
        return 0.0;
    }
    let active: f64 = samples
        .into_iter()
        .map(|sample| sample.active_seconds)
        .sum();
    active / span_seconds
}

/// Whether `metric` names the Active Session History LOAD series — the series
/// `DescribeDimensionKeys` takes as its `Metric` and `GetResourceMetrics` reports from the
/// sampled sessions — rather than an engine or OS counter.
///
/// The name carries an aggregate function ("to find the average for the metric db.load you
/// must use db.load.avg"), which is stripped before the series is read off it.
#[must_use]
pub fn names_the_load_series(metric: &str) -> bool {
    let base = super::AGGREGATE_FUNCTION_SUFFIXES
        .iter()
        .find_map(|suffix| metric.strip_suffix(suffix))
        .unwrap_or(metric);
    matches!(base, LOAD_METRIC | SAMPLED_LOAD_METRIC)
}

/// `Metric`'s "db.load.avg - A scaled representation of the number of active sessions for
/// the database engine", without its aggregate function.
pub const LOAD_METRIC: &str = "db.load";

/// `Metric`'s "db.sampledload.avg - The raw number of active sessions for the database
/// engine", without its aggregate function.
pub const SAMPLED_LOAD_METRIC: &str = "db.sampledload";

/// The dimensions a sample can fill for the dimension group `group`, or `None` for a group
/// whose dimensions name nothing a sample holds.
///
/// Which groups EXIST is what `ListAvailableResourceDimensions` enumerates (its
/// `DIMENSION_GROUP_CATALOG`, built from the `DimensionGroup$Group` documentation); this is
/// the subset whose dimensions the recorded activity of a session answers — who was
/// connected, from where, to which database, running which statement, and what the session
/// was waiting on. A group outside it (the blocking-session groups, Amazon DocumentDB's
/// query groups) holds no dimension this emulator's engines record, so a request grouping by
/// one reports no keys rather than keys filled from something else.
#[must_use]
pub fn group_dimensions(group: &str) -> Option<&'static [&'static str]> {
    PROJECTED_GROUPS
        .iter()
        .find(|(name, _)| *name == group)
        .map(|(_, dimensions)| *dimensions)
}

/// The dimension groups [`group_dimensions`] answers for, each with its own dimensions in
/// the order `ListAvailableResourceDimensions` publishes them.
const PROJECTED_GROUPS: &[(&str, &[&str])] = &[
    ("db", &["db.name"]),
    ("db.application", &["db.application.name"]),
    ("db.host", &["db.host.id", "db.host.name"]),
    ("db.session_type", &["db.session_type.name"]),
    (
        "db.sql",
        &[
            "db.sql.db_id",
            "db.sql.id",
            "db.sql.statement",
            "db.sql.tokenized_id",
        ],
    ),
    (
        "db.sql_tokenized",
        &[
            "db.sql_tokenized.db_id",
            "db.sql_tokenized.id",
            "db.sql_tokenized.statement",
        ],
    ),
    ("db.user", &["db.user.id", "db.user.name"]),
    ("db.wait_event", &["db.wait_event.name", "db.wait_event.type"]),
    ("db.wait_event_type", &["db.wait_event_type.name"]),
];

/// The value the dimension `name` has for `sample`, or `None` when the sample holds none —
/// a dimension of a group whose values this emulator's engines do not record, or a value
/// the session did not have (an application name a client never declared).
///
/// The one projection, so the dimensions a key is GROUPED by, the ones it is PARTITIONED by
/// and the ones a `Filter` selects on cannot read a sample differently.
#[must_use]
pub fn dimension_value(name: &str, sample: &ActivitySample) -> Option<String> {
    let value = match name {
        // The database the session was connected to, which is what `db.name` is and what
        // the per-SQL groups report as the statement's database.
        "db.name" | "db.sql.db_id" | "db.sql_tokenized.db_id" => sample.database.clone(),
        "db.application.name" => sample.application.clone(),
        "db.host.name" => sample.host.clone(),
        "db.host.id" => stable_id(&sample.host),
        // `db.session_type.name`: a session a client opened is a normal one, as against the
        // engine's own background processes, which this emulator runs none of.
        "db.session_type.name" => CLIENT_SESSION_TYPE.to_string(),
        "db.sql.statement" => sample.statement.clone(),
        "db.sql.id" => stable_id(&sample.statement),
        "db.sql.tokenized_id" | "db.sql_tokenized.id" => stable_id(&sample.statement_digest),
        "db.sql_tokenized.statement" => sample.statement_digest.clone(),
        "db.user.name" => sample.user.clone(),
        "db.user.id" => stable_id(&sample.user),
        "db.wait_event.name" => sample.wait_event.name.clone(),
        "db.wait_event.type" | "db.wait_event_type.name" => sample.wait_event.event_type.clone(),
        _ => return None,
    };
    // `DimensionMap`'s values are `RequestString` (pattern `\S`), so a dimension the session
    // had no value for is absent rather than present and empty.
    (!value.trim().is_empty()).then_some(value)
}

/// `db.session_type.name` for a session a CLIENT opened, which is every session this
/// emulator's engines serve.
const CLIENT_SESSION_TYPE: &str = "normal";

/// The id Performance Insights reports for a dimension VALUE — a statement, a user, a host.
///
/// The service reports an id beside the value for those dimensions, and the id identifies
/// the value: it is a digest OF the value, so two reads of the same statement answer the
/// same id, two different statements answer different ids, and no id is minted per request.
#[must_use]
pub fn stable_id(value: &str) -> String {
    use sha2::Digest as _;

    let mut hasher = sha2::Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

/// How many dimension keys, and how many partition keys, ONE answer reports.
///
/// The modelled bound of both `DimensionGroup$Limit` ("request that Performance Insights
/// return a limited number of values for a dimension") and `MaxResults` is 25, so a caller
/// that names either is answered at most that many; this is the same bound applied when it
/// names NEITHER, which is what keeps the top-N answer — and the per-key breakdown, one
/// value per partition key — bounded by the response shape rather than by how many distinct
/// sessions a window happened to hold.
pub const MAX_REPORTED_KEYS: usize = 25;

/// One group of samples: the dimensions they share, and the load they account for.
#[derive(Debug, Clone)]
pub struct DimensionKey {
    /// The dimensions of the group, as the response reports them.
    pub dimensions: BTreeMap<String, String>,
    /// The average active sessions this group accounted for over the window.
    pub total: f64,
    /// The same figure per partition, in the order the partition keys are reported, for a
    /// request that asked for a breakdown.
    pub partitions: Vec<f64>,
}

/// The dimension keys `samples` hold for the group `group`, ordered by the load each
/// accounted for, DESCENDING.
///
/// * `dimensions` restricts which of the group's dimensions are reported, which is what
///   `DimensionGroup$Dimensions` does ("Performance Insights returns all dimensions within
///   this group, unless you provide the names of specific dimensions within this group"); a
///   list naming none of the group's own dimensions restricts nothing.
/// * `partition_keys` are the partition dimensions each key's load is also broken down by,
///   already ordered, which is what `PartitionBy` asks for.
/// * `span_seconds` is the window's own length, which the load is averaged over.
///
/// Ties are broken by the key's own dimensions, so a window whose groups weigh the same is
/// still reported in one order rather than an arbitrary one.
#[must_use]
pub fn dimension_keys(
    samples: &[ActivitySample],
    group: &str,
    dimensions: Option<&[String]>,
    partition_keys: &[BTreeMap<String, String>],
    span_seconds: f64,
) -> Vec<DimensionKey> {
    let Some(group_dimensions) = group_dimensions(group) else {
        return Vec::new();
    };
    let reported: Vec<&str> = match dimensions {
        Some(asked) => {
            let restricted: Vec<&str> = group_dimensions
                .iter()
                .filter(|name| asked.iter().any(|one| one == *name))
                .copied()
                .collect();
            match restricted.is_empty() {
                true => group_dimensions.to_vec(),
                false => restricted,
            }
        }
        None => group_dimensions.to_vec(),
    };
    // One entry per distinct set of reported dimensions, each holding the samples that share
    // it. `BTreeMap` so the grouping is by VALUE and the order is the keys' own.
    let mut grouped: BTreeMap<BTreeMap<String, String>, Vec<&ActivitySample>> = BTreeMap::new();
    for sample in samples {
        let mut key = BTreeMap::new();
        for name in &reported {
            if let Some(value) = dimension_value(name, sample) {
                key.insert((*name).to_string(), value);
            }
        }
        // A sample that fills none of the group's dimensions is activity this group cannot
        // place, so it is not reported under an empty key.
        if key.is_empty() {
            continue;
        }
        grouped.entry(key).or_default().push(sample);
    }
    let mut keys: Vec<DimensionKey> = grouped
        .into_iter()
        .map(|(dimensions, samples)| DimensionKey {
            partitions: partition_keys
                .iter()
                .map(|partition| {
                    let inside = samples
                        .iter()
                        .copied()
                        .filter(|sample| matches_dimensions(sample, partition));
                    average_active_sessions(inside, span_seconds)
                })
                .collect(),
            total: average_active_sessions(samples.iter().copied(), span_seconds),
            dimensions,
        })
        .collect();
    // "the top N dimension keys for a metric": ordered by the load each accounted for, the
    // largest first.
    keys.sort_by(|left, right| {
        right
            .total
            .partial_cmp(&left.total)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.dimensions.cmp(&right.dimensions))
    });
    keys
}

/// Whether `sample` has every dimension value in `dimensions`.
///
/// Used both for the partition a sample falls in and for the `Filter` a request selects
/// with: a filter naming a dimension the sample has no value for selects it out, because the
/// sample does not have that value.
#[must_use]
pub fn matches_dimensions(
    sample: &ActivitySample,
    dimensions: &BTreeMap<String, String>,
) -> bool {
    dimensions
        .iter()
        .all(|(name, value)| dimension_value(name, sample).as_deref() == Some(value.as_str()))
}

/// The PARTITION KEYS `samples` hold for the group `group`, ordered by the load each
/// accounts for, descending, and cut to `limit` — the caller's, or
/// [`MAX_REPORTED_KEYS`] where it named none.
///
/// "For each dimension specified in GroupBy, specify a secondary dimension to further
/// subdivide the partition keys in the response" — the partitions are the distinct values of
/// that secondary group inside the window, which is what each key's own breakdown is aligned
/// to.
#[must_use]
pub fn partition_keys(
    samples: &[ActivitySample],
    group: &str,
    dimensions: Option<&[String]>,
    limit: Option<usize>,
    span_seconds: f64,
) -> Vec<BTreeMap<String, String>> {
    let mut keys: Vec<BTreeMap<String, String>> =
        dimension_keys(samples, group, dimensions, &[], span_seconds)
            .into_iter()
            .map(|key| key.dimensions)
            .collect();
    keys.truncate(limit.unwrap_or(MAX_REPORTED_KEYS).min(MAX_REPORTED_KEYS));
    keys
}

#[cfg(test)]
mod tests {
    use ::service_common::db_activity_samples::WaitEvent;

    use super::*;

    /// A sample of `active_seconds` waiting on `wait_event`, run by `user`.
    fn sample(user: &str, wait_event: &str, event_type: &str, active_seconds: f64) -> ActivitySample {
        ActivitySample {
            at_epoch_seconds: 1_000.0,
            active_seconds,
            wait_event: WaitEvent {
                name: wait_event.to_string(),
                event_type: event_type.to_string(),
            },
            session_id: 11,
            user: user.to_string(),
            host: "127.0.0.1".to_string(),
            database: "appdb".to_string(),
            application: String::new(),
            statement: "SELECT 1".to_string(),
            statement_digest: "SELECT ?".to_string(),
        }
    }

    #[test]
    fn keys_are_ordered_by_the_load_each_group_accounted_for() {
        let samples = vec![
            sample("alice", "CPU", "CPU", 1.0),
            sample("bob", "Lock:transactionid", "Lock", 4.0),
            sample("alice", "CPU", "CPU", 2.0),
        ];
        let keys = dimension_keys(&samples, "db.wait_event", None, &[], 10.0);
        let totals: Vec<f64> = keys.iter().map(|key| key.total).collect();
        assert_eq!(
            totals,
            vec![0.4, 0.3],
            "case two wait events: expected the heavier first, got {totals:?}"
        );
        assert_eq!(
            keys[0].dimensions.get("db.wait_event.name").cloned(),
            Some("Lock:transactionid".to_string()),
            "case two wait events: expected the lock wait on top, got {:?}",
            keys[0].dimensions
        );
        assert_eq!(
            keys[0].dimensions.get("db.wait_event.type").cloned(),
            Some("Lock".to_string()),
            "case two wait events: expected every dimension of the group, got {:?}",
            keys[0].dimensions
        );
    }

    #[test]
    fn a_restricted_dimension_list_reports_only_what_it_names() {
        let samples = vec![sample("alice", "CPU", "CPU", 1.0)];
        let asked = vec!["db.user.name".to_string()];
        let keys = dimension_keys(&samples, "db.user", Some(&asked), &[], 10.0);
        assert_eq!(
            keys.len(),
            1,
            "case one user: expected one key, got {}",
            keys.len()
        );
        assert_eq!(
            keys[0].dimensions.keys().collect::<Vec<_>>(),
            vec!["db.user.name"],
            "case a restricted list: expected only the named dimension, got {:?}",
            keys[0].dimensions
        );
    }

    #[test]
    fn a_group_no_sample_can_fill_reports_no_keys() {
        let samples = vec![sample("alice", "CPU", "CPU", 1.0)];
        for group in ["db.blocking_session", "db.query", "not.a.group"] {
            let keys = dimension_keys(&samples, group, None, &[], 10.0);
            assert!(
                keys.is_empty(),
                "case the group {group}: expected no keys, got {keys:?}"
            );
        }
    }

    #[test]
    fn a_partition_breaks_each_key_down_in_the_partitions_own_order() {
        let samples = vec![
            sample("alice", "CPU", "CPU", 1.0),
            sample("alice", "Lock:transactionid", "Lock", 3.0),
            sample("bob", "CPU", "CPU", 2.0),
        ];
        let partitions = partition_keys(&samples, "db.wait_event_type", None, None, 10.0);
        let names: Vec<Option<&String>> = partitions
            .iter()
            .map(|key| key.get("db.wait_event_type.name"))
            .collect();
        assert_eq!(
            names.len(),
            2,
            "case two event types: expected two partitions, got {names:?}"
        );
        let keys = dimension_keys(&samples, "db.user", None, &partitions, 10.0);
        let alice = keys
            .iter()
            .find(|key| key.dimensions.get("db.user.name").map(String::as_str) == Some("alice"))
            .expect("case the busiest user: it is reported");
        assert_eq!(
            alice.partitions.len(),
            partitions.len(),
            "case a partitioned key: expected one value per partition, got {:?}",
            alice.partitions
        );
        let summed: f64 = alice.partitions.iter().sum();
        assert!(
            (summed - alice.total).abs() < 1e-9,
            "case a partitioned key: expected the partitions to sum to the total {}, got {:?}",
            alice.total,
            alice.partitions
        );
    }

    #[test]
    fn the_load_series_is_named_with_an_aggregate_function() {
        // Each row is a `Metric` value and whether it names the sampled load series.
        let rows: &[(&str, bool)] = &[
            ("db.load.avg", true),
            ("db.sampledload.avg", true),
            ("db.load.sum", true),
            ("db.load", true),
            ("os.cpuUtilization.total.avg", false),
            ("db.SQL.Innodb_rows_read.avg", false),
        ];
        for (metric, expected) in rows {
            assert_eq!(
                names_the_load_series(metric),
                *expected,
                "case {metric}: expected {expected}"
            );
        }
    }

    #[test]
    fn an_id_identifies_its_value_and_nothing_else() {
        let first = stable_id("SELECT 1");
        assert_eq!(
            first,
            stable_id("SELECT 1"),
            "case the same value twice: expected the same id"
        );
        assert_ne!(
            first,
            stable_id("SELECT 2"),
            "case two values: expected different ids"
        );
    }
}
