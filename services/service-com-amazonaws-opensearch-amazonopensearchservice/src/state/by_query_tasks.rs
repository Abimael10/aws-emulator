// SPDX-License-Identifier: CC0-1.0

//! The BY-QUERY WRITES one domain's cluster runs in the BACKGROUND, and the report every
//! by-query write — background or not — is answered from.
//!
//! A by-query write is asked for in one of two forms, and which one is the request's own
//! `wait_for_completion`: a request that WAITS is answered with the finished report, and a
//! request that does not is answered with a HANDLE to the work and nothing else, while the
//! write goes on in the background and the client watches it through that handle. The record
//! below is that work: the documents the query selected when the write was submitted, how
//! many of them have been removed so far, and the figures the report is composed of.
//!
//! The work is paced by the CLOCK, and nothing sleeps for it: the request's own
//! `requests_per_second` says how many documents a second the write may remove, the instant
//! the whole run falls due is stored on the record, and the documents that pace has made due
//! are removed by whichever later call reads the domain ([`Domain::settle_by_query_tasks`],
//! reached from [`Domain::settle`]). So a client that polls the handle, one that counts the
//! index and one that does neither all see the same documents gone at the same instant —
//! what the clock owed was owed whether or not anybody looked.
//!
//! The figures a run reports are its own ([`ByQueryReport`]), and one reading of them serves
//! both forms: the body a waiting request is answered with, the `status` a poll of the handle
//! reports, and the `response` a finished task carries are three renderings of ONE set of
//! counts, so no two of them can describe one run differently.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, SystemTime};

use ::serde_json::{json, Map, Value};

use super::{epoch_millis, Domain, Index};

/// How many documents one batch of a by-query write's scroll carries when the request names
/// no `scroll_size` of its own: the engine's own default batch size, the one its throttling
/// documentation reckons with ("By default the batch size is 1000").
pub const DEFAULT_SCROLL_SIZE: usize = 1_000;

/// What a by-query answer reports for `requests_per_second` when the request asked for no
/// throttle: "-1 … means no throttle", which is both the value that disables throttling and
/// the one an unthrottled run reports back.
pub const UNTHROTTLED_REQUESTS_PER_SECOND: f64 = -1.0;

/// The least time a by-query write a client OFFLOADED takes before the clock owes its
/// completion.
///
/// The same kind of span [`super::DOMAIN_WORK`] is, chosen for the same reason: it is
/// deliberately NOT zero, so a client that polls the handle straight after the submit still
/// observes the run in flight — the `completed: false` and the partial `status.deleted` the
/// asynchronous form exists to be watched through. A request that asked for a THROTTLE
/// slower than this paces by the throttle instead ([`ByQueryTask::paced`]), because the rate
/// the caller asked for is the rate the write runs at.
pub const TASK_WORK: Duration = Duration::from_secs(3);

/// The longest a throttle a request names may stretch one run over.
///
/// A rate approaching zero names a run approaching forever, and the instant such a run falls
/// due has to be one this process can hold: past this the span is this bound, which a client
/// observes as a run still going.
const MAX_TASK_SPAN: Duration = Duration::from_secs(24 * 60 * 60);

/// How many by-query task records one domain keeps.
///
/// The emulator's OWN bound on this process's state, like [`super::INDEX_QUOTA`]: a client
/// offloading one write after another must not grow the store without end, so a FINISHED
/// record is dropped to make room for a new one ([`Domain::register_by_query_task`]) and a
/// cluster already running this many of them takes no more.
pub const TASK_QUOTA: usize = 50;

/// What the engine calls the kind of task a by-query write runs as — every one of them is
/// carried over the cluster's transport layer.
const TASK_TYPE: &str = "transport";

/// One by-query write running in the background of a cluster.
///
/// The documents it removes are the ones the query selected WHEN IT WAS SUBMITTED: a
/// by-query write takes its subset from one scroll opened at the start, so the run removes
/// exactly the documents that scroll named and a document written afterwards is not one of
/// them. They are held as the index and id of each, which is what a removal needs, and each
/// is dropped from the record as the run reaches it — so a run that has finished holds its
/// counts and no document list at all.
#[derive(Debug, Clone)]
pub struct ByQueryTask {
    /// The id of the cluster node running it, which is the first half of the handle a client
    /// polls it by.
    pub node: String,
    /// The number this node gave the task, which is the second half of that handle. Counted
    /// per domain from [`Domain::take_by_query_task_number`], so no two runs of one cluster
    /// share a handle.
    pub number: u64,
    /// The engine action this is a run of — `indices:data/write/delete/byquery` for a
    /// delete-by-query, which a poll reports so a client can tell what the handle names.
    pub action: &'static str,
    /// How the engine describes the work, which a poll reports beside the action.
    pub description: String,
    /// When the request that submitted it was served: what the run's own elapsed time and
    /// the instant its completion falls due are counted from.
    pub started_at: SystemTime,
    /// How long the whole run takes: the throttle the request asked for where that is the
    /// slower of the two, and [`TASK_WORK`] otherwise.
    pub span: Duration,
    /// How much of that span the THROTTLE accounts for, which is what the run reports as the
    /// time it was held back; zero for a run nothing throttled.
    pub throttled: Duration,
    /// `scroll_size`: the documents one batch of the run's scroll carries, which is what its
    /// batch count is reckoned in.
    pub scroll_size: usize,
    /// `requests_per_second` as the request named it, or
    /// [`UNTHROTTLED_REQUESTS_PER_SECOND`] for a request that named none.
    pub requests_per_second: f64,
    /// How many documents the query selected when the run was submitted.
    pub total: usize,
    /// How many of them the run has reached so far.
    pub processed: usize,
    /// How many of those were really removed — a document another request had already
    /// deleted is one this run reached and removed nothing for.
    pub deleted: usize,
    /// The documents the run has still to reach, as the index each sits in and its id, in the
    /// order the scroll named them.
    pub pending: VecDeque<(String, String)>,
    /// Whether the clock has owed this run's completion: once it has, every document the
    /// scroll named has been reached and the run reports the report it came to.
    pub completed: bool,
}

/// What the request that submits a background by-query write asked of it, apart from the
/// documents its query selected: the identity the cluster gives the run, the engine action it
/// is a run of and how it is paced.
///
/// One value rather than a parameter list, because these are what the SUBMITTING route reads
/// off the request and hands over whole ([`ByQueryTask::submitted`]).
#[derive(Debug, Clone)]
pub struct SubmittedByQuery {
    /// The node the run is taken on, which is the first half of its handle.
    pub node: String,
    /// The number that node gave it, which is the second half.
    pub number: u64,
    /// The engine action the run is of.
    pub action: &'static str,
    /// How the engine describes the work.
    pub description: String,
    /// `scroll_size`: the documents one batch of the run's scroll carries.
    pub scroll_size: usize,
    /// `requests_per_second`: the rate the run may remove them at.
    pub requests_per_second: f64,
}

impl ByQueryTask {
    /// The by-query write `documents` names, submitted at `now` as `asked` describes it.
    #[must_use]
    pub fn submitted(
        asked: SubmittedByQuery,
        documents: VecDeque<(String, String)>,
        now: SystemTime,
    ) -> Self {
        let SubmittedByQuery {
            node,
            number,
            action,
            description,
            scroll_size,
            requests_per_second,
        } = asked;
        let total = documents.len();
        let throttled = Self::paced(total, requests_per_second);
        Self {
            node,
            number,
            action,
            description,
            started_at: now,
            span: throttled.max(TASK_WORK),
            throttled,
            scroll_size,
            requests_per_second,
            total,
            processed: 0,
            deleted: 0,
            pending: documents,
            completed: false,
        }
    }

    /// The handle a client polls this run by: `<node>:<id>`, the two halves of its identity
    /// with the separator between them.
    #[must_use]
    pub fn handle(&self) -> String {
        format!("{}:{}", self.node, self.number)
    }

    /// How long removing `total` documents takes at `requests_per_second`, and no time at all
    /// for a rate that throttles nothing.
    ///
    /// The throttle "pads each batch with a wait time" so that the documents a batch carries
    /// take `batch / requests_per_second` seconds: over the whole run that is one second per
    /// `requests_per_second` documents, whatever the batch size.
    #[must_use]
    pub fn paced(total: usize, requests_per_second: f64) -> Duration {
        if !requests_per_second.is_finite() || requests_per_second <= 0.0 {
            return Duration::ZERO;
        }
        let seconds = total as f64 / requests_per_second;
        Duration::try_from_secs_f64(seconds)
            .unwrap_or(MAX_TASK_SPAN)
            .min(MAX_TASK_SPAN)
    }

    /// How many of the documents the scroll named this run has reached by `now`: the pace the
    /// run goes at, applied to the time it has been going.
    fn due_processed(&self, now: SystemTime) -> usize {
        let elapsed = now.duration_since(self.started_at).unwrap_or_default();
        if elapsed >= self.span {
            return self.total;
        }
        let span = self.span.as_millis().max(1);
        let due = self.total as u128 * elapsed.as_millis() / span;
        usize::try_from(due).unwrap_or(self.total).min(self.total)
    }

    /// Remove the documents the clock has made due, and mark the run finished once its whole
    /// span has passed.
    ///
    /// Each document removed takes a sequence number of the index it sat in, exactly as a
    /// delete of that document by its own id would: the run is a WRITE, and a client reading
    /// the index afterwards can tell the revision it is at.
    pub fn settle(&mut self, indexes: &mut BTreeMap<String, Index>, now: SystemTime) {
        if self.completed {
            return;
        }
        let due = self.due_processed(now);
        while self.processed < due {
            let Some((index_name, id)) = self.pending.pop_front() else {
                break;
            };
            self.processed += 1;
            let Some(index) = indexes.get_mut(&index_name) else {
                continue;
            };
            if index.documents.remove(&id).is_some() {
                let _seq_no = index.take_seq_no();
                self.deleted += 1;
            }
        }
        if now.duration_since(self.started_at).unwrap_or_default() >= self.span {
            self.completed = true;
        }
    }

    /// The figures this run has come to as they stand at `now`.
    #[must_use]
    pub fn figures(&self, now: SystemTime) -> ByQueryReport {
        let ran = self.ran_for(now);
        ByQueryReport {
            took: ran,
            // A background run reaches every document the scroll named — what stops it is the
            // end of its own subset — so it never stops with documents left unreached.
            timed_out: false,
            total: self.total,
            deleted: self.deleted,
            processed: self.processed,
            scroll_size: self.scroll_size,
            requests_per_second: self.requests_per_second,
            throttled: ran.min(self.throttled),
        }
    }

    /// How long the run has been going at `now`, which stops at the span the whole run takes.
    fn ran_for(&self, now: SystemTime) -> Duration {
        now.duration_since(self.started_at)
            .unwrap_or_default()
            .min(self.span)
    }

    /// What a poll of this run's handle is answered with: whether it has finished, the task
    /// itself with the counts it has reached, and — once it has finished — the report it came
    /// to.
    #[must_use]
    pub fn polled(&self, now: SystemTime) -> Value {
        let figures = self.figures(now);
        let mut answer = Map::new();
        answer.insert("completed".to_string(), Value::Bool(self.completed));
        answer.insert(
            "task".to_string(),
            json!({
                // The two halves of the handle, so a client can recompose the handle it
                // polled from the task it was answered with.
                "node": self.node,
                "id": self.number,
                "type": TASK_TYPE,
                "action": self.action,
                "status": figures.status(),
                "description": self.description,
                "start_time_in_millis": epoch_millis(self.started_at),
                "running_time_in_nanos": u64::try_from(self.ran_for(now).as_nanos())
                    .unwrap_or(u64::MAX),
                "cancellable": true,
                "cancelled": false,
                "headers": Map::new(),
            }),
        );
        if self.completed {
            answer.insert("response".to_string(), figures.rendered());
        }
        Value::Object(answer)
    }
}

/// The figures ONE by-query write came to, apart from how they are rendered.
///
/// Held as a value because the same counts are reported three ways — the body a request that
/// waited for the write is answered with, the `status` a poll of a background run reports,
/// and the `response` a finished run carries — so none of them can state a run's outcome
/// differently from another.
#[derive(Debug, Clone, Copy)]
pub struct ByQueryReport {
    /// How long the work took.
    pub took: Duration,
    /// Whether it stopped with documents it had selected but not reached.
    pub timed_out: bool,
    /// How many documents the query selected.
    pub total: usize,
    /// How many of them were removed.
    pub deleted: usize,
    /// How many of them the scroll paged through, which is what the batch count is of.
    pub processed: usize,
    /// The size of one batch of that scroll, as the request named it.
    pub scroll_size: usize,
    /// The throttle the request asked for, reported back as the rate the work ran at.
    pub requests_per_second: f64,
    /// How long that throttle held the work back.
    pub throttled: Duration,
}

impl ByQueryReport {
    /// `batches`: how many batches the scroll paged the documents in — the documents it
    /// paged through over the batch size the request asked for, rounded up, since a part
    /// batch is still a batch.
    #[must_use]
    pub fn batches(&self) -> usize {
        self.processed.div_ceil(self.scroll_size.max(1))
    }

    /// The report itself: the body a request that WAITED for the write is answered with, and
    /// the `response` a finished background run carries.
    #[must_use]
    pub fn rendered(&self) -> Value {
        json!({
            "took": millis(self.took),
            "timed_out": self.timed_out,
            "total": self.total,
            "deleted": self.deleted,
            "batches": self.batches(),
            // Nothing this emulator's writes can produce: no document is written under a
            // version this run did not expect, no removal leaves a document untouched, and
            // nothing is retried.
            "version_conflicts": 0,
            "noops": 0,
            "retries": { "bulk": 0, "search": 0 },
            "throttled_millis": millis(self.throttled),
            "requests_per_second": self.requests_per_second,
            // Nothing is waiting to be let through: the run is paced by the instant each
            // batch falls due rather than by a padding it is inside of.
            "throttled_until_millis": 0,
            "failures": Vec::<Value>::new(),
        })
    }

    /// The `status` of the task running the write, which is the same figures as the report
    /// with what the task itself answers around them left off — and with the other two things
    /// a by-query write can do to a document, which a DELETE does to none of them.
    #[must_use]
    pub fn status(&self) -> Value {
        json!({
            "total": self.total,
            "updated": 0,
            "created": 0,
            "deleted": self.deleted,
            "batches": self.batches(),
            "version_conflicts": 0,
            "noops": 0,
            "retries": { "bulk": 0, "search": 0 },
            "throttled_millis": millis(self.throttled),
            "requests_per_second": self.requests_per_second,
            "throttled_until_millis": 0,
        })
    }
}

/// `of` as the millisecond count a by-query report states its durations in.
fn millis(of: Duration) -> u64 {
    u64::try_from(of.as_millis()).unwrap_or(u64::MAX)
}

impl Domain {
    /// Materialise what the clock owes the by-query writes this domain is running: every
    /// document their pace has made due is removed, and a run whose whole span has passed is
    /// finished.
    ///
    /// Reached from [`Domain::settle`], so ANY read of this domain materialises it: the
    /// removals were owed at the same instant whether or not a client polled for them.
    pub fn settle_by_query_tasks(&mut self, now: SystemTime) {
        if self.by_query_tasks.is_empty() {
            return;
        }
        // Taken off the domain for the walk, because each run writes into the domain's own
        // indexes — one record cannot be borrowed twice.
        let mut running = ::std::mem::take(&mut self.by_query_tasks);
        for task in running.values_mut() {
            task.settle(&mut self.indexes, now);
        }
        self.by_query_tasks = running;
    }

    /// The task number the next by-query write this cluster takes on is given: every run
    /// takes the next number, so no two handles of one cluster are the same and a handle a
    /// client holds keeps naming the run it was minted for.
    pub fn take_by_query_task_number(&mut self) -> u64 {
        let taken = self.next_by_query_task_number;
        self.next_by_query_task_number = self.next_by_query_task_number.saturating_add(1);
        taken
    }

    /// Store `task` and hand back the handle a client polls it by.
    ///
    /// The records are bounded by [`TASK_QUOTA`]: past it the oldest FINISHED run is dropped,
    /// which is the record a client has already had the report of. Nothing in flight is
    /// dropped — a run still removing documents is work the cluster owes — so how many of
    /// those there can be is the submitting route's own to refuse
    /// ([`Domain::by_query_tasks_running`]).
    pub fn register_by_query_task(&mut self, task: ByQueryTask) -> String {
        let handle = task.handle();
        self.by_query_tasks.insert(handle.clone(), task);
        while self.by_query_tasks.len() > TASK_QUOTA {
            let oldest = self
                .by_query_tasks
                .values()
                .filter(|task| task.completed)
                .min_by_key(|task| task.started_at)
                .map(ByQueryTask::handle);
            let Some(oldest) = oldest else {
                break;
            };
            self.by_query_tasks.remove(&oldest);
        }
        handle
    }

    /// How many by-query writes this domain's cluster is running right now — read after
    /// [`Domain::settle`] has materialised whatever the clock owed them, so a run whose span
    /// has passed is no longer one of them.
    #[must_use]
    pub fn by_query_tasks_running(&self) -> usize {
        self.by_query_tasks
            .values()
            .filter(|task| !task.completed)
            .count()
    }
}
