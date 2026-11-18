// SPDX-License-Identifier: CC0-1.0

//! The RUN of a replication task: the full load it performs, the accounting that load
//! records, and the stop it arrives at on its own.
//!
//! A task DMS starts does not sit in one status for ever. It carries out its migration
//! type's phases and reports what each phase moved — `ReplicationTask$Status`,
//! `ReplicationTask$StopReason`, `ReplicationTask$ReplicationTaskStats` and, per table,
//! `TableStatistics` — so this module owns the one description of a run:
//!
//! * what the clock owes a started task ([`TaskRun`]): the full load becomes due once the
//!   task is `running`, and a task whose migration type has NO change-data-capture phase
//!   to follow stops itself when that load finishes, with the reason
//!   [`FULL_LOAD_ONLY_FINISHED`];
//! * what the full load actually MOVED ([`TableLoad`]), which is read off the endpoints'
//!   own data plane rather than assumed: an Amazon S3 source holds a table's full-load
//!   objects under `<BucketFolder>/<schema>/<table>/`, and an Amazon S3 target takes the
//!   rows of such a table as the single object [`FULL_LOAD_OBJECT_NAME`] under its own
//!   bucket and folder. A table the source does not hold loads no rows and is reported as
//!   one the migration found absent, never as one that moved;
//! * the projections both readings of that accounting go through
//!   ([`ReplicationTaskRecord::stats`], [`ReplicationTaskRecord::table_statistics`]), so
//!   `DescribeReplicationTasks` and `DescribeTableStatistics` answer from the same run.
//!
//! The object store is reached the way any emulated service reaches another: the AWS SDK
//! driven over the emulator's own in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), through the one shared
//! construction of that hop ([`service_common::s3_client`]). Nothing here opens a socket,
//! resolves a name or reads a host file, and every read is bounded — a listing, the
//! objects one table is loaded from, and each object's bytes all have ceilings below.
//!
//! [`advance_task_runs`] is what DRIVES a run: every operation that reports or acts on a
//! replication task calls it first, so the work the clock has already made due has
//! happened before any status, statistic or stop reason is read.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use ::smithy_server_runtime::EmbeddedEmulator;

use super::{
    tstamp, ComAmazonawsDatabasemigrationserviceAmazondmsv20160101State, EndpointRecord,
    ReplicationTaskRecord, DEFAULT_REGION, TASK_RUNNING, TASK_STOPPED, TRANSITION_DELAY,
};
use crate::ops::types::{ReplicationTaskStats, TableStatistics};

/// The `MigrationTypeValue` whose task runs a full load and nothing after it.
pub const MIGRATION_FULL_LOAD: &str = "full-load";
/// The `MigrationTypeValue` whose task runs a full load and then keeps replicating
/// changes.
pub const MIGRATION_FULL_LOAD_AND_CDC: &str = "full-load-and-cdc";

/// Why a task that had only a full load to do stopped: "\"Stop Reason
/// FULL_LOAD_ONLY_FINISHED\" – The task completed the full load phase"
/// (`ReplicationTask$StopReason`). Fixed text, with no variable part.
pub const FULL_LOAD_ONLY_FINISHED: &str = "Stop Reason FULL_LOAD_ONLY_FINISHED";

/// The `TableStatistics$TableState` of a table whose load has not begun. One of the
/// member's documented states: "Valid states: Table does not exist | Before load | Full
/// load | Table completed | Table cancelled | Table error | Table is being reloaded".
pub const TABLE_BEFORE_LOAD: &str = "Before load";
/// The state of a table the full load is moving right now.
pub const TABLE_FULL_LOAD: &str = "Full load";
/// The state of a table whose full load finished.
pub const TABLE_COMPLETED: &str = "Table completed";
/// The state of a table the migration found no data for at the source.
pub const TABLE_DOES_NOT_EXIST: &str = "Table does not exist";

/// The `StartReplicationTaskTypeValue` that reloads the target, which makes the load a
/// RELOAD of each table rather than a new one (`TableStatistics$FullLoadReloaded`).
pub const START_TYPE_RELOAD_TARGET: &str = "reload-target";
/// The `StartReplicationTaskTypeValue` that picks a previous run up where it stopped, so
/// the run is not a fresh start (`ReplicationTaskStats$FreshStartDate`).
pub const START_TYPE_RESUME_PROCESSING: &str = "resume-processing";

/// The `EngineName` whose endpoints this emulator holds the data plane of: an Amazon S3
/// bucket, which its own object store serves. A full load moves rows only between two
/// such endpoints — there is no database server behind an endpoint of any other engine, so
/// no rows could be read from or written to one.
const S3_ENGINE: &str = "s3";

/// The ONE object a table's full load writes at an Amazon S3 target: DMS names a full-load
/// file `LOAD` followed by its sequence number, and one load of one table is the first and
/// only file of that sequence.
pub const FULL_LOAD_OBJECT_NAME: &str = "LOAD00000001.csv";

/// The file-name prefix of the full-load objects a table is READ from at an Amazon S3
/// source — the same `LOAD` sequence, whatever number each file carries.
const FULL_LOAD_OBJECT_PREFIX: &str = "LOAD";

/// The most tables ONE full load moves.
///
/// A RESOURCE GUARD, not a published DMS quota: each table is read into memory and written
/// out again, so a source holding more tables than this is loaded up to this many rather
/// than without bound.
const MAX_LOADED_TABLES: usize = 100;

/// The most object keys one listing of a source bucket reads — the same guard, on the
/// enumeration that discovers which tables the source holds.
const MAX_LISTED_OBJECTS: usize = 1_000;

/// The most full-load objects one table is read from.
const MAX_OBJECTS_PER_TABLE: usize = 100;

/// The most bytes read from ONE source object, and so the largest full-load file this
/// emulator moves.
const MAX_OBJECT_BYTES: usize = 1 << 20;

/// The most bytes one table's full load carries in total, across its objects.
const MAX_TABLE_BYTES: usize = 8 << 20;

/// The most selection rules one table-mappings document is read for — a bound on the
/// caller-supplied document, applied before any of its rules is matched.
const MAX_SELECTION_RULES: usize = 100;

/// The most characters a selection rule's `schema-name` or `table-name` pattern may carry
/// for this emulator to match it.
///
/// A RESOURCE GUARD, not a published DMS quota: a pattern is matched against every table
/// name the source holds, so its length multiplies that work. It sits at the longest
/// identifier the database engines DMS migrates accept, so no name a real schema carries is
/// affected.
const MAX_LOCATOR_PATTERN_LEN: usize = 128;

/// The most characters of a key segment that are read as a table or schema NAME — the same
/// guard on the other operand of a match.
const MAX_TABLE_NAME_LEN: usize = 128;

/// The most candidate tables ONE load matches its selection rules against, before the
/// tables it selects are capped by [`MAX_LOADED_TABLES`] — the same guard on how many
/// matches a bucket's contents can ask for.
const MAX_CANDIDATE_TABLES: usize = 200;

/// The character steps ONE load spends matching its patterns, across every candidate and
/// every rule: the bound on the PRODUCT of the two lengths and the two counts above. A load
/// that spends it selects no further table ([`Matcher`]).
const MAX_MATCH_STEPS: u64 = 1_000_000;

/// How many candidates one pass matches before yielding to the runtime, so a cancellation
/// or a shutdown can land in the one CPU-bound stretch of a load.
const CANDIDATES_PER_YIELD: usize = 16;

/// The most full loads ONE pass of [`advance_task_runs`] performs, so the work a single
/// request drives is bounded however many tasks are due at once. What is left stays due,
/// and the next call that reports a task performs it.
const MAX_LOADS_PER_PASS: usize = 8;

/// One run of a replication task: what `StartReplicationTask` started, and what the clock
/// owes it from there.
///
/// Every instant is STORED rather than slept on, and the instant a transition was OWED at
/// is the one reported for it, so two readings of the same finished run agree whenever
/// they happen.
#[derive(Debug, Clone)]
pub struct TaskRun {
    /// When `StartReplicationTask` started the run.
    pub started: SystemTime,
    /// Whether the run is a fresh start or a target reload rather than a resume, which is
    /// what `ReplicationTaskStats$FreshStartDate` reports a date for: "The date the
    /// replication task was started either with a fresh start or a target reload."
    pub fresh: bool,
    /// Whether each table is being RELOADED rather than loaded by a new full load, which
    /// is `TableStatistics$FullLoadReloaded`.
    pub reloaded: bool,
    /// When the run's full load becomes due, or `None` for a migration type that has no
    /// full-load phase at all (`cdc`).
    pub full_load_due: Option<SystemTime>,
    /// The full load, once it has run.
    pub full_load: Option<FullLoad>,
    /// How long the task has been running, which is `ReplicationTaskStats$ElapsedTimeMillis`.
    /// Materialised by [`ReplicationTaskRecord::settle_run`] from the stored instants.
    pub elapsed: Duration,
    /// When the run stopped, once it has.
    pub stopped: Option<SystemTime>,
}

impl TaskRun {
    /// The run `StartReplicationTask` begins at `now` for a task of `migration_type`,
    /// started as `start_type`.
    pub fn starting(now: SystemTime, migration_type: &str, start_type: &str) -> Self {
        Self {
            started: now,
            fresh: start_type != START_TYPE_RESUME_PROCESSING,
            reloaded: start_type == START_TYPE_RELOAD_TARGET,
            // The load runs once the task is `running`, which is the status the clock owes
            // it one span after the start.
            full_load_due: loads_tables(migration_type).then(|| now + TRANSITION_DELAY),
            full_load: None,
            elapsed: Duration::ZERO,
            stopped: None,
        }
    }
}

/// The full load of one run: when it ran, when it is finished, and what it moved.
#[derive(Debug, Clone)]
pub struct FullLoad {
    /// When the load started — the instant it was due, which is when the task reached
    /// `running`.
    pub started: SystemTime,
    /// When the load is finished, and so when a task with no further phase stops.
    pub finish_due: SystemTime,
    /// Whether the clock has carried the run past [`Self::finish_due`].
    pub finished: bool,
    /// The tables the load covered, one entry per table the task's table mappings select.
    pub tables: Vec<TableLoad>,
}

/// What ONE table's full load moved.
#[derive(Debug, Clone)]
pub struct TableLoad {
    /// The schema the table belongs to, as the task's table mappings name it.
    pub schema_name: String,
    /// The table's name.
    pub table_name: String,
    /// How many rows the load moved out of the source and into the target.
    pub rows: i64,
    /// How many rows failed to load.
    pub errored_rows: i64,
    /// Whether the source held the table at all. A table the mappings name and the source
    /// does not hold is reported as absent, not as loaded.
    pub present_at_source: bool,
}

/// Whether a task of `migration_type` has a full-load phase at all.
fn loads_tables(migration_type: &str) -> bool {
    migration_type == MIGRATION_FULL_LOAD || migration_type == MIGRATION_FULL_LOAD_AND_CDC
}

/// Whether a task of `migration_type` STOPS when its full load finishes: one whose only
/// phase was that load, since a task that also captures changes keeps running.
fn stops_after_full_load(migration_type: &str) -> bool {
    migration_type == MIGRATION_FULL_LOAD
}

impl ReplicationTaskRecord {
    /// Materialise what the clock owes this task's run at `now`.
    ///
    /// Called from [`super::ComAmazonawsDatabasemigrationserviceAmazondmsv20160101State::settle`],
    /// so a read and a later read agree: the elapsed time of a running task, and the
    /// finish of a full load whose instant has passed — which, for a task with no phase
    /// after it, is also the stop that carries [`FULL_LOAD_ONLY_FINISHED`].
    pub(crate) fn settle_run(&mut self, now: SystemTime) {
        let running = self.status == TASK_RUNNING;
        let stops = stops_after_full_load(&self.migration_type);
        let Some(run) = self.run.as_mut() else {
            return;
        };
        if running {
            run.elapsed = now.duration_since(run.started).unwrap_or_default();
        }
        let Some(load) = run.full_load.as_mut() else {
            return;
        };
        if load.finished || now < load.finish_due {
            return;
        }
        load.finished = true;
        if stops {
            // The load was the whole task, so the task stops itself at the instant the
            // load finished — and the elapsed time it reports is frozen there too.
            run.stopped = Some(load.finish_due);
            run.elapsed = load
                .finish_due
                .duration_since(run.started)
                .unwrap_or_default();
            self.status = TASK_STOPPED.to_string();
            self.stop_reason = Some(FULL_LOAD_ONLY_FINISHED.to_string());
        }
    }

    /// The statistics of this task's run, as `ReplicationTask$ReplicationTaskStats`, or
    /// `None` for a task that has never run — which has no statistics to report rather
    /// than zeroed ones.
    pub fn stats(&self) -> Option<ReplicationTaskStats> {
        let run = self.run.as_ref()?;
        let load = run.full_load.as_ref();
        let finished = load.is_some_and(|load| load.finished);
        let loaded = load
            .map(|load| {
                load.tables
                    .iter()
                    .filter(|table| table.present_at_source)
                    .count()
            })
            .unwrap_or_default();
        Some(ReplicationTaskStats {
            // The load is reported complete only once every table it covers is, which
            // happens at the one instant the load finishes.
            full_load_progress_percent: if finished { 100 } else { 0 },
            elapsed_time_millis: i64::try_from(run.elapsed.as_millis()).unwrap_or(i64::MAX),
            tables_loaded: if finished {
                i32::try_from(loaded).unwrap_or(i32::MAX)
            } else {
                0
            },
            tables_loading: if finished {
                0
            } else {
                i32::try_from(loaded).unwrap_or(i32::MAX)
            },
            // Nothing here defers a table: every table the load covers is read in the one
            // pass that performs it, so none is ever waiting its turn.
            tables_queued: 0,
            tables_errored: i32::try_from(
                load.map(|load| {
                    load.tables
                        .iter()
                        .filter(|table| table.errored_rows > 0)
                        .count()
                })
                .unwrap_or_default(),
            )
            .unwrap_or(i32::MAX),
            fresh_start_date: run.fresh.then(|| tstamp(run.started)),
            start_date: Some(tstamp(run.started)),
            stop_date: run.stopped.map(tstamp),
            full_load_start_date: load.map(|load| tstamp(load.started)),
            full_load_finish_date: load
                .filter(|load| load.finished)
                .map(|load| tstamp(load.finish_due)),
        })
    }

    /// This task's per-table accounting, as `DescribeTableStatistics` reports it: one
    /// entry per table the run's full load covers, in schema and table order.
    ///
    /// A task that has never run, and one whose load has not begun, has nothing per table
    /// to report yet.
    pub fn table_statistics(&self) -> Vec<TableStatistics> {
        let Some(load) = self.run.as_ref().and_then(|run| run.full_load.as_ref()) else {
            return Vec::new();
        };
        let reloaded = self.run.as_ref().is_some_and(|run| run.reloaded);
        let mut statistics: Vec<TableStatistics> = load
            .tables
            .iter()
            .map(|table| TableStatistics {
                schema_name: Some(table.schema_name.clone()),
                table_name: Some(table.table_name.clone()),
                full_load_rows: table.rows,
                full_load_error_rows: table.errored_rows,
                full_load_start_time: table.present_at_source.then(|| tstamp(load.started)),
                full_load_end_time: (table.present_at_source && load.finished)
                    .then(|| tstamp(load.finish_due)),
                full_load_reloaded: table.present_at_source.then_some(reloaded),
                last_update_time: Some(if load.finished {
                    tstamp(load.finish_due)
                } else {
                    tstamp(load.started)
                }),
                table_state: Some(table_state(table, load).to_string()),
                ..TableStatistics::default()
            })
            .collect();
        statistics.sort_by(|left, right| {
            (&left.schema_name, &left.table_name).cmp(&(&right.schema_name, &right.table_name))
        });
        statistics
    }
}

/// The `TableState` of `table` under `load`: a table the source does not hold exists
/// nowhere to load from, one whose load has finished is complete, and one the load is
/// still carrying out is in its full load.
fn table_state(table: &TableLoad, load: &FullLoad) -> &'static str {
    if !table.present_at_source {
        TABLE_DOES_NOT_EXIST
    } else if load.finished {
        TABLE_COMPLETED
    } else {
        TABLE_FULL_LOAD
    }
}

/// One table's full load that the clock has made due, with everything performing it
/// needs — read off the store under its lock, so the load itself runs with nothing held.
#[derive(Debug, Clone)]
struct DueFullLoad {
    /// The task the load belongs to.
    task_arn: String,
    /// The Region the task's resources live in, which addresses the object store.
    region: String,
    /// When the load started, and when it is finished.
    started: SystemTime,
    finish_due: SystemTime,
    /// The task's table mappings, which select the tables to load.
    table_mappings: String,
    /// The Amazon S3 side the rows are read from, when the source endpoint is one.
    source: Option<S3Side>,
    /// The Amazon S3 side they are written to, when the target endpoint is one.
    target: Option<S3Side>,
}

/// One Amazon S3 side of a full load: the bucket a table's objects sit in, and the folder
/// prefix they sit under.
#[derive(Debug, Clone)]
struct S3Side {
    /// The bucket `S3Settings$BucketName` names.
    bucket: String,
    /// `S3Settings$BucketFolder`, normalised to either nothing or a prefix ending in `/`.
    folder: String,
}

/// The Amazon S3 side `endpoint` is, when it is one: an endpoint of the `s3` engine
/// carrying a bucket name.
fn s3_side(endpoint: Option<&EndpointRecord>) -> Option<S3Side> {
    let endpoint = endpoint?;
    if endpoint.engine_name != S3_ENGINE {
        return None;
    }
    let settings = endpoint.settings.s3.as_ref()?;
    let bucket = settings.bucket_name.as_deref()?.trim();
    if bucket.is_empty() {
        return None;
    }
    let folder = settings
        .bucket_folder
        .as_deref()
        .unwrap_or_default()
        .trim()
        .trim_matches('/');
    Some(S3Side {
        bucket: bucket.to_string(),
        folder: if folder.is_empty() {
            String::new()
        } else {
            format!("{folder}/")
        },
    })
}

impl ComAmazonawsDatabasemigrationserviceAmazondmsv20160101State {
    /// The next full load the clock has made due, if any task has one.
    ///
    /// A load is due once its task is `running` — the status the clock owes a started task
    /// — and the instant it was scheduled for has passed. A task whose endpoints this
    /// store no longer holds still runs its load: there is nothing for it to move, and a
    /// load that could never run would leave its task `running` for ever.
    fn next_due_full_load(&self, now: SystemTime) -> Option<DueFullLoad> {
        let task = self.replication_tasks.values().find(|task| {
            task.status == TASK_RUNNING
                && task.run.as_ref().is_some_and(|run| {
                    run.full_load.is_none()
                        && run.full_load_due.is_some_and(|due| now >= due)
                })
        })?;
        let started = task.run.as_ref().and_then(|run| run.full_load_due)?;
        let region = ::service_common::arn::region_of(&task.arn, "dms")
            .and_then(::service_common::arn::region_label)
            .unwrap_or(DEFAULT_REGION)
            .to_string();
        Some(DueFullLoad {
            task_arn: task.arn.clone(),
            region,
            started,
            finish_due: started + TRANSITION_DELAY,
            table_mappings: task.table_mappings.clone(),
            source: s3_side(self.endpoints.get(&task.source_endpoint_arn)),
            target: s3_side(self.endpoints.get(&task.target_endpoint_arn)),
        })
    }

    /// Record what the full load `due` names moved, unless another pass of the same load
    /// already did — both reach the same record, so the first one to land settles it.
    fn record_full_load(&mut self, due: &DueFullLoad, tables: Vec<TableLoad>) {
        let Some(task) = self.replication_tasks.get_mut(&due.task_arn) else {
            return;
        };
        let Some(run) = task.run.as_mut() else {
            return;
        };
        if run.full_load.is_some() || run.full_load_due != Some(due.started) {
            return;
        }
        run.full_load = Some(FullLoad {
            started: due.started,
            finish_due: due.finish_due,
            finished: false,
            tables,
        });
    }
}

/// Perform every full load the clock has already made due.
///
/// Called by each operation that reports or acts on a replication task, with that
/// handler's own engine handle and the in-process callback the load reaches the object
/// store through. One load runs per pass of the loop, and the store's lock is released for
/// the duration of each one, so no object read or write happens while another request is
/// shut out.
pub async fn advance_task_runs<Eng: Send + Sync + 'static>(
    engine: &Eng,
    callback: &EmbeddedEmulator,
    now: SystemTime,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    for _ in 0..MAX_LOADS_PER_PASS {
        let due = {
            let mut store = state.lock();
            store.settle(now);
            store.next_due_full_load(now)
        };
        let Some(due) = due else { return };
        let tables = run_full_load(callback, &due).await;
        {
            let mut store = state.lock();
            store.record_full_load(&due, tables);
            store.settle(now);
        }
    }
}

/// Move the tables the load `due` covers, and report what each one moved.
///
/// Rows move only between two Amazon S3 endpoints: those are the endpoints whose data
/// plane this emulator holds, so for any other pair there is no source to read and no
/// target to write, and the load covers no table at all rather than reporting tables it
/// did not move.
async fn run_full_load(callback: &EmbeddedEmulator, due: &DueFullLoad) -> Vec<TableLoad> {
    let (Some(source), Some(target)) = (due.source.as_ref(), due.target.as_ref()) else {
        ::tracing::debug!(
            task = %due.task_arn,
            "a full load's endpoints are not a pair this emulator holds the data plane of, \
             so it moves no rows"
        );
        return Vec::new();
    };
    let selections = selections(&due.table_mappings);
    if selections.is_empty() {
        return Vec::new();
    }
    let client = ::service_common::s3_client::s3_client(callback, &due.region);
    let sides = Sides {
        region: &due.region,
        source,
        target,
    };
    let held = source_tables(&client, source).await;

    // The tables the load covers: every table the source holds that the mappings select,
    // plus every table a rule names outright, so a table the mappings ask for and the
    // source does not hold is reported rather than silently dropped. Both the number of
    // candidates and the matching itself are bounded ([`Matcher`]).
    let mut matcher = Matcher::new();
    let mut covered: Vec<(String, String)> = Vec::new();
    for (position, (schema, table)) in held.keys().take(MAX_CANDIDATE_TABLES).enumerate() {
        // Matching is the one stretch of a load that only computes, and it runs on a
        // runtime worker: yielding through it keeps the process answering.
        if position % CANDIDATES_PER_YIELD == 0 {
            ::tokio::task::yield_now().await;
        }
        if matcher.spent() || covered.len() >= MAX_LOADED_TABLES {
            break;
        }
        if matcher.selects(&selections, schema, table) {
            covered.push((schema.clone(), table.clone()));
        }
    }
    for selection in &selections {
        if matcher.spent() || covered.len() >= MAX_LOADED_TABLES {
            break;
        }
        if !selection.include || selection.has_wildcard() {
            continue;
        }
        let named = selection.named();
        if !covered.contains(&named) && matcher.selects(&selections, &named.0, &named.1) {
            covered.push(named);
        }
    }
    covered.sort();

    let mut loaded: Vec<TableLoad> = Vec::with_capacity(covered.len());
    for (schema, table) in covered {
        let rows = match held.get(&(schema.clone(), table.clone())) {
            Some(keys) => load_table(callback, &client, sides, &schema, &table, keys).await,
            // A table the mappings name and the source holds no full-load object for.
            None => None,
        };
        loaded.push(TableLoad {
            schema_name: schema,
            table_name: table,
            rows: rows.unwrap_or_default(),
            // Nothing in a move between two object stores can reject a row: a row the
            // source served is a row the target took.
            errored_rows: 0,
            present_at_source: rows.is_some(),
        });
    }
    loaded
}

/// Read one table's full-load objects off the source side and write its rows to the
/// target, answering how many rows moved — or `None` when the source served none of its
/// objects, which is a table this load did not move.
///
/// The two Amazon S3 sides one full load moves between, and the Region they are addressed
/// in — the pair every table of that load is read off and written to.
#[derive(Debug, Clone, Copy)]
struct Sides<'a> {
    region: &'a str,
    source: &'a S3Side,
    target: &'a S3Side,
}

/// Read one table's full-load objects off the source side and write its rows to the
/// target, answering how many rows moved — or `None` when the source served none of its
/// objects, which is a table this load did not move.
///
/// The rows of one table arrive at the target as the ONE full-load object
/// [`FULL_LOAD_OBJECT_NAME`] under that table's own prefix, whatever number of files the
/// source held them in, and the bytes written are the bytes read: this moves a table's
/// rows, it does not render them.
async fn load_table(
    callback: &EmbeddedEmulator,
    client: &::aws_sdk_s3::Client,
    sides: Sides<'_>,
    schema: &str,
    table: &str,
    keys: &[String],
) -> Option<i64> {
    let (source, target) = (sides.source, sides.target);
    let mut rows: Vec<u8> = Vec::new();
    let mut served = false;
    for key in keys.iter().take(MAX_OBJECTS_PER_TABLE) {
        // The one read of an object this tree states, which also says what the store's
        // answer about it ESTABLISHED: only bytes it served are rows to move.
        let read = ::service_common::s3_object_read::read_object(
            callback,
            sides.region,
            &source.bucket,
            key,
            None,
            MAX_OBJECT_BYTES,
        )
        .await;
        let ::service_common::s3_object_read::ObjectRead::Served(bytes) = read else {
            ::tracing::debug!(
                bucket = %source.bucket,
                key = %key,
                answer = ?read,
                "a full load could not read one of a table's source objects"
            );
            continue;
        };
        served = true;
        if rows.len().saturating_add(bytes.len()) > MAX_TABLE_BYTES {
            ::tracing::debug!(
                table = %table,
                "a table holds more full-load bytes than one load moves"
            );
            break;
        }
        // One file's last row and the next file's first row stay separate rows.
        if !rows.is_empty() && !rows.ends_with(b"\n") {
            rows.push(b'\n');
        }
        rows.extend_from_slice(&bytes);
    }
    if !served {
        return None;
    }
    let moved = row_count(&rows);
    let key = format!(
        "{folder}{schema}/{table}/{FULL_LOAD_OBJECT_NAME}",
        folder = target.folder
    );
    let written = client
        .put_object()
        .bucket(&target.bucket)
        .key(&key)
        .body(::aws_sdk_s3::primitives::ByteStream::from(rows))
        .send()
        .await;
    if let Err(error) = written {
        ::tracing::debug!(
            error = ?error,
            bucket = %target.bucket,
            key = %key,
            "a full load could not write a table's rows to the target"
        );
        // The rows did not arrive, so none moved.
        return None;
    }
    Some(moved)
}

/// How many rows `body` holds: one per line that carries anything, since a delimited
/// full-load file ends each row with a line break and a trailing break closes the last
/// row rather than opening another.
fn row_count(body: &[u8]) -> i64 {
    let rows = body
        .split(|byte| *byte == b'\n')
        .filter(|row| row.iter().any(|byte| !byte.is_ascii_whitespace()))
        .count();
    i64::try_from(rows).unwrap_or(i64::MAX)
}

/// One selection rule of a table-mappings document: the schema and table patterns its
/// object locator names, and whether it brings those tables INTO the migration.
///
/// Each pattern is held as its CHARACTERS, converted once when the document is read rather
/// than on every table name the rule is matched against.
#[derive(Debug, Clone)]
struct Selection {
    /// The `schema-name` pattern.
    schema: Vec<char>,
    /// The `table-name` pattern.
    table: Vec<char>,
    /// Whether the rule's action includes the tables it matches.
    include: bool,
}

impl Selection {
    /// Whether either pattern carries a wildcard, so the rule names no single table.
    fn has_wildcard(&self) -> bool {
        self.schema
            .iter()
            .chain(self.table.iter())
            .any(|character| *character == '%' || *character == '_')
    }

    /// The schema and table a rule with no wildcard names outright.
    fn named(&self) -> (String, String) {
        (
            self.schema.iter().collect(),
            self.table.iter().collect(),
        )
    }
}

/// The selection rules `table_mappings` carries.
///
/// The document is JSON a caller supplied, so it is READ by this tree's JSON parser rather
/// than scanned, and only its selection rules are read: "rules" is a list of rule objects,
/// each with a "rule-type", an "object-locator" naming a "schema-name" and a "table-name",
/// and a "rule-action". A document that is not readable as that carries no selection, so
/// no table is loaded for it — and neither does one carrying a pattern longer than
/// [`MAX_LOCATOR_PATTERN_LEN`], since dropping the one rule could widen what the others
/// select.
fn selections(table_mappings: &str) -> Vec<Selection> {
    let Ok(document) = ::serde_json::from_str::<::serde_json::Value>(table_mappings) else {
        ::tracing::debug!("a task's table mappings are not a readable JSON document");
        return Vec::new();
    };
    let Some(rules) = document.get("rules").and_then(::serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut selections: Vec<Selection> = Vec::new();
    for rule in rules.iter().take(MAX_SELECTION_RULES) {
        if rule.get("rule-type").and_then(::serde_json::Value::as_str) != Some(RULE_TYPE_SELECTION)
        {
            continue;
        }
        let Some(locator) = rule.get("object-locator") else {
            continue;
        };
        let (Some(schema), Some(table)) = (
            locator
                .get("schema-name")
                .and_then(::serde_json::Value::as_str),
            locator
                .get("table-name")
                .and_then(::serde_json::Value::as_str),
        ) else {
            continue;
        };
        let (schema, table): (Vec<char>, Vec<char>) =
            (schema.chars().collect(), table.chars().collect());
        if schema.len() > MAX_LOCATOR_PATTERN_LEN || table.len() > MAX_LOCATOR_PATTERN_LEN {
            ::tracing::debug!(
                "a task's table mappings carry a pattern longer than this load matches, so \
                 the document selects no table"
            );
            return Vec::new();
        }
        let action = rule
            .get("rule-action")
            .and_then(::serde_json::Value::as_str)
            .unwrap_or(RULE_ACTION_INCLUDE);
        selections.push(Selection {
            schema,
            table,
            include: action != RULE_ACTION_EXCLUDE,
        });
    }
    selections
}

/// The `rule-type` of a rule that says which tables the task migrates.
const RULE_TYPE_SELECTION: &str = "selection";
/// The `rule-action` that brings the tables a selection rule matches into the migration.
const RULE_ACTION_INCLUDE: &str = "include";
/// The `rule-action` that leaves them out of it.
const RULE_ACTION_EXCLUDE: &str = "exclude";

/// The pattern matching of ONE full load, and the budget that bounds it.
///
/// Both operands of a match are caller-influenced — the pattern comes out of the task's
/// table mappings, the name out of a key the source bucket holds — and a `%` pattern is
/// matched by giving the last wildcard one more character, which costs up to
/// `len(pattern) * len(name)` steps. The lengths are capped where each value is read
/// ([`MAX_LOCATOR_PATTERN_LEN`], [`MAX_TABLE_NAME_LEN`]) and the number of matches by
/// [`MAX_CANDIDATE_TABLES`]; this budget bounds their PRODUCT, so no document and no
/// bucket can make one load's matching run long. A load that spends it selects no further
/// table.
#[derive(Debug)]
struct Matcher {
    /// The character steps left to spend.
    budget: u64,
}

impl Matcher {
    /// A matcher with the budget one load gets.
    fn new() -> Self {
        Self {
            budget: MAX_MATCH_STEPS,
        }
    }

    /// Whether the budget is spent, so nothing further is matched.
    fn spent(&self) -> bool {
        self.budget == 0
    }

    /// Whether the mappings select the table `schema`.`table`: some rule that includes it
    /// matches it, and no rule that excludes one does.
    fn selects(&mut self, selections: &[Selection], schema: &str, table: &str) -> bool {
        let schema: Vec<char> = schema.chars().take(MAX_TABLE_NAME_LEN).collect();
        let table: Vec<char> = table.chars().take(MAX_TABLE_NAME_LEN).collect();
        let mut included = false;
        for selection in selections {
            if !(self.like(&selection.schema, &schema) && self.like(&selection.table, &table)) {
                continue;
            }
            // An exclusion of the table settles it whatever else matched.
            if !selection.include {
                return false;
            }
            included = true;
        }
        included
    }

    /// Whether `value` matches the pattern `pattern`, where `%` stands for any run of
    /// characters and `_` for exactly one — the wildcards a table-mapping object locator's
    /// names carry.
    ///
    /// Matched iteratively, with the position of the last `%` remembered rather than
    /// recursed on, so a pattern a caller supplied cannot grow the stack, and every
    /// iteration spends one step of the budget, so it cannot spin either.
    fn like(&mut self, pattern: &[char], value: &[char]) -> bool {
        let (mut at_pattern, mut at_value) = (0_usize, 0_usize);
        let mut wildcard: Option<usize> = None;
        let mut wildcard_at_value = 0_usize;
        while at_value < value.len() {
            if self.budget == 0 {
                return false;
            }
            self.budget -= 1;
            match pattern.get(at_pattern) {
                Some('%') => {
                    wildcard = Some(at_pattern);
                    wildcard_at_value = at_value;
                    at_pattern += 1;
                }
                Some('_') => {
                    at_pattern += 1;
                    at_value += 1;
                }
                Some(character) if Some(character) == value.get(at_value) => {
                    at_pattern += 1;
                    at_value += 1;
                }
                // The pattern does not match here: give the last `%` one more character, or
                // report no match when there was none to give.
                _ => match wildcard {
                    Some(position) => {
                        at_pattern = position + 1;
                        wildcard_at_value += 1;
                        at_value = wildcard_at_value;
                    }
                    None => return false,
                },
            }
        }
        pattern
            .get(at_pattern..)
            .is_some_and(|rest| rest.iter().all(|character| *character == '%'))
    }
}

/// The full-load objects the source side holds, by the table they belong to.
///
/// A table's objects sit at `<BucketFolder>/<schema>/<table>/LOAD*`, so the listing's keys
/// are what says which tables the source holds. A key that does not have that shape — one
/// nested deeper, one naming a folder, one whose file is not a full-load file — belongs to
/// no table's full load and is left alone.
async fn source_tables(
    client: &::aws_sdk_s3::Client,
    source: &S3Side,
) -> BTreeMap<(String, String), Vec<String>> {
    let mut held: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut read = 0_usize;
    let mut continuation: Option<String> = None;
    loop {
        let mut listing = client.list_objects_v2().bucket(&source.bucket);
        if !source.folder.is_empty() {
            listing = listing.prefix(&source.folder);
        }
        if let Some(token) = continuation.as_deref() {
            listing = listing.continuation_token(token);
        }
        let page = match listing.send().await {
            Ok(page) => page,
            Err(error) => {
                ::tracing::debug!(
                    error = ?error,
                    bucket = %source.bucket,
                    "a full load's source bucket could not be listed"
                );
                return held;
            }
        };
        for object in page.contents() {
            let Some(key) = object.key() else { continue };
            read += 1;
            if let Some(table) = full_load_object(key, &source.folder) {
                held.entry(table).or_default().push(key.to_string());
            }
            if read >= MAX_LISTED_OBJECTS {
                return held;
            }
        }
        continuation = page.next_continuation_token().map(ToString::to_string);
        if continuation.is_none() {
            return held;
        }
    }
}

/// The table `key` holds a full-load object of, when it holds one: a key of the form
/// `<folder><schema>/<table>/LOAD…`.
///
/// A segment longer than [`MAX_TABLE_NAME_LEN`] names no table of a database engine, so
/// such a key belongs to no table's full load either.
fn full_load_object(key: &str, folder: &str) -> Option<(String, String)> {
    let relative = key.strip_prefix(folder)?;
    let mut parts = relative.split('/');
    let schema = parts.next()?;
    let table = parts.next()?;
    let file = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if schema.is_empty()
        || table.is_empty()
        || schema.len() > MAX_TABLE_NAME_LEN
        || table.len() > MAX_TABLE_NAME_LEN
        || !file.starts_with(FULL_LOAD_OBJECT_PREFIX)
    {
        return None;
    }
    Some((schema.to_string(), table.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a table-mapping object locator's names are patterns, where `%` stands for any
    /// run of characters and `_` for exactly one, and a match costs a bounded number of
    /// steps whatever the pattern.
    #[test]
    fn a_locator_name_matches_as_a_pattern() {
        let rows: &[(&str, &str, &str, bool)] = &[
            ("an exact name", "sales", "sales", true),
            ("a different name", "sales", "stock", false),
            ("every table", "%", "orders", true),
            ("a prefix", "sales_%", "sales_2026", true),
            ("a prefix that does not hold", "sales_%", "stock_2026", false),
            ("a suffix", "%_archive", "orders_archive", true),
            ("one character", "orders_____", "orders_2026", true),
            ("one character too few", "orders____", "orders_2026", false),
            ("an inner wildcard", "or%rs", "orders", true),
            ("nothing at all for a wildcard", "orders%", "orders", true),
        ];
        for (case, pattern, value, expected) in rows {
            let pattern: Vec<char> = pattern.chars().collect();
            let value: Vec<char> = value.chars().collect();
            let actual = Matcher::new().like(&pattern, &value);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {pattern:?} against {value:?} to be {expected}, got \
                 {actual}"
            );
        }
    }

    /// RULE: the matching of one load is bounded: a pattern of repeated wildcards against a
    /// long name spends the budget and then selects nothing, rather than running on.
    #[test]
    fn matching_stops_when_its_step_budget_is_spent() {
        let pattern: Vec<char> = "%a%a%a%a%a%a%a%a%a%a%a%a%a%a%b".chars().collect();
        let value: Vec<char> = "a".repeat(MAX_TABLE_NAME_LEN).chars().collect();
        let mut matcher = Matcher::new();
        let matched = matcher.like(&pattern, &value);
        assert!(
            !matched,
            "case \"a pattern that cannot match\": expected no match, got one"
        );
        // Whatever the match cost, one load cannot be made to spend more than its budget:
        // once spent, every further match is a no-match.
        let mut exhausted = Matcher { budget: 0 };
        let exact: Vec<char> = "orders".chars().collect();
        assert_eq!(
            (exhausted.spent(), exhausted.like(&exact, &exact)),
            (true, false),
            "case \"a spent budget\": expected a spent matcher to select nothing"
        );
    }

    /// RULE: a delimited full-load file holds one row per line that carries anything, so a
    /// trailing line break closes the last row rather than opening another.
    #[test]
    fn rows_are_the_lines_that_carry_something() {
        let rows: &[(&str, &[u8], i64)] = &[
            ("one row with a trailing break", b"1,alice\n", 1),
            ("one row with no trailing break", b"1,alice", 1),
            ("three rows", b"1,alice\n2,bob\n3,cleo\n", 3),
            ("a blank line between rows", b"1,alice\n\n2,bob\n", 2),
            ("carriage returns", b"1,alice\r\n2,bob\r\n", 2),
            ("no rows at all", b"", 0),
            ("nothing but line breaks", b"\n\n", 0),
        ];
        for (case, body, expected) in rows {
            let actual = row_count(body);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected} rows, got {actual}"
            );
        }
    }

    /// RULE: a key names a table's full load only at `<folder><schema>/<table>/LOAD…`, and
    /// a name longer than this load reads belongs to no table.
    ///
    /// A row's expectation is the table the key names as `<schema>.<table>`, or nothing.
    #[test]
    fn a_full_load_key_names_its_table() {
        let overlong = "x".repeat(MAX_TABLE_NAME_LEN + 1);
        let overlong_key = format!("{overlong}/orders/LOAD00000001.csv");
        let rows: &[(&str, &str, &str, Option<&str>)] = &[
            (
                "a table under a folder",
                "stage/sales/orders/LOAD00000001.csv",
                "stage/",
                Some("sales.orders"),
            ),
            (
                "a table under no folder",
                "sales/orders/LOAD00000001.csv",
                "",
                Some("sales.orders"),
            ),
            (
                "a second full-load file of the same table",
                "sales/orders/LOAD00000002.csv",
                "",
                Some("sales.orders"),
            ),
            (
                "a file that is not a full-load file",
                "sales/orders/20260101-120000.csv",
                "",
                None,
            ),
            ("a folder marker", "sales/orders/", "", None),
            (
                "an object nested deeper",
                "sales/orders/extra/LOAD00000001.csv",
                "",
                None,
            ),
            (
                "an object outside the folder",
                "other/sales/orders/LOAD1.csv",
                "stage/",
                None,
            ),
            ("a schema name past the bound", &overlong_key, "", None),
        ];
        for (case, key, folder, expected) in rows {
            let actual = full_load_object(key, folder).map(|(schema, table)| format!("{schema}.{table}"));
            assert_eq!(
                actual.as_deref(),
                *expected,
                "case {case:?}: expected the key to name {expected:?}, got {actual:?}"
            );
        }
    }
}
