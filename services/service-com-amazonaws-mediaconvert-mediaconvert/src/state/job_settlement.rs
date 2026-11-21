// SPDX-License-Identifier: CC0-1.0

//! The transitions the CLOCK owes this store, and the one place each is materialised: a
//! step of a job's run, a jobs query's completion, a job's share attempt reaching `SHARED`,
//! and a job's hop to one of its hop destinations.
//!
//! Nothing here decides WHEN anything moves: the due instants the records carry do
//! ([`ComAmazonawsMediaconvertMediaconvertState::settle`] applies exactly what was owed at
//! the instant it is handed), so a caller reading before one still observes the transient
//! state and its state errors. Which status the end of a job's run reaches is decided by
//! that job's own RUN ([`super::JobRunRecord`]), made while the job is `PROGRESSING`.
//!
//! Split out of `state.rs`, which holds the store and its records: these are the writes the
//! clock makes to them.

use super::*;

impl ComAmazonawsMediaconvertMediaconvertState {
    /// Materialise every status change the clock had already made due at `now` — a step of
    /// a job's run (its exit from `SUBMITTED`, or the end of a run already `PROGRESSING`),
    /// a jobs query's completion, a job's share attempt reaching `SHARED` and a job's hop
    /// to one of its hop destinations.
    ///
    /// Bounded by the number of records actually due, not by the size of the store: every
    /// pending index is ordered by due instant, so this drains their already-due prefix,
    /// oldest first across the four, and stops at the first entry still in the future. Each
    /// drained entry costs a bounded, constant amount of work — a job's status and timing,
    /// a query's status flip, a share attempt's status flip or one move of a job between
    /// two queues — and the one entry a step of a run writes in its place is the NEXT step
    /// of that same run, of which a job has one, so the cost of
    /// one call is the number of records that came due and nothing else; no kind re-reads
    /// the store. It is the only write a reporting operation makes, and every change it
    /// applies was owed at `now` whether or not anybody called.
    ///
    /// WHICH handlers call it: only those whose own request path reads or writes the state
    /// this drains — a job's status or timing, a queue projection's job counts, a jobs
    /// query, a share attempt or a hop. A door onto a resource the clock owes nothing
    /// (a preset, a job template, a policy, a record's tag set, the version catalogue)
    /// does not settle: the job lifecycle is not its business, and nothing it reports or
    /// stores could differ either way.
    ///
    /// It is also what the store's own clock pump calls ([`drive_job_clock`]), at the
    /// instants the due instants below name, so a transition — and the EventBridge event
    /// that reports it — happens with no request waiting on it: this service is an event
    /// source, and the client that drives work off a job's events makes no further
    /// MediaConvert call at all. A reader arriving later therefore finds the change already
    /// materialised rather than materialising it itself, which is the same state either
    /// way, because every change applied here was owed at the instant it is applied.
    ///
    /// Whether a job may leave `SUBMITTED` at all is its QUEUE's decision, not the
    /// clock's: `QueueStatus` says "If you pause a queue, jobs in that queue won't
    /// begin", so a job whose queue is `PAUSED` keeps `SUBMITTED` and is left with no due
    /// instant until [`Self::rearm_queue`] gives it a fresh one when the queue is
    /// `ACTIVE` again. See [`Self::settle_job`].
    pub fn settle(&mut self, now: SystemTime) {
        loop {
            // The four timelines are drained as one, oldest due instant first, so each
            // change lands in the order the clock made it due. `min_by_key` over the
            // head of each index keeps that ordering without a match arm per pair.
            let next = [
                self.pending.first().map(|entry| (entry.clone(), Due::Job)),
                self.pending_queries
                    .first()
                    .map(|entry| (entry.clone(), Due::JobsQuery)),
                self.pending_shares
                    .first()
                    .map(|entry| (entry.clone(), Due::Share)),
                self.pending_hops
                    .first()
                    .map(|entry| (entry.clone(), Due::Hop)),
            ]
            .into_iter()
            .flatten()
            .min_by(|left, right| left.0.cmp(&right.0));
            // Nothing is pending at all, or the earliest entry is still in the future.
            let Some((entry, kind)) = next.filter(|(entry, _)| entry.0 <= now) else {
                break;
            };
            match kind {
                Due::Job => {
                    self.pending.remove(&entry);
                    self.settle_job(entry.0, &entry.1, now);
                }
                Due::JobsQuery => {
                    self.pending_queries.remove(&entry);
                    self.complete_jobs_query(&entry.1);
                }
                Due::Share => {
                    self.pending_shares.remove(&entry);
                    self.complete_share(entry.0, &entry.1);
                }
                Due::Hop => {
                    self.pending_hops.remove(&entry);
                    self.hop_job(entry.0, &entry.1);
                }
            }
        }
    }

    /// Whether `queue_name` names a queue this store holds in `PAUSED`.
    ///
    /// A job whose queue is gone is not held: the queue that could hold it back no longer
    /// exists, and `DeleteQueue` already refuses a queue with jobs in a non-terminal
    /// status, so this is only ever a job whose queue outlived it.
    pub(super) fn queue_is_paused(&self, queue_name: &str) -> bool {
        self.queues
            .get(queue_name)
            .is_some_and(|queue| queue.status == QUEUE_STATUS_PAUSED)
    }

    /// Give every job still waiting in `queue_name` a fresh due instant for its exit from
    /// `SUBMITTED`.
    ///
    /// `QueueStatus`: a paused queue's jobs "won't begin". So a job held while its queue
    /// was paused begins only once the queue is `ACTIVE` again, and the span it then waits
    /// is the same one every other job waits — the time the queue spent paused does not
    /// count towards it. Called by `UpdateQueue` when it sets a queue's status to
    /// `ACTIVE`. Every still-`SUBMITTED` job in the queue is re-armed, including one
    /// whose own due instant elapsed unobserved during the pause (nothing drained
    /// `pending` while it was paused), so no job transitions on the first read after the
    /// unpause on the strength of an instant that passed while it was held.
    pub fn rearm_queue(&mut self, queue_name: &str, now: SystemTime) {
        let due = now + self.job_settles_after;
        let waiting: Vec<(String, Option<SystemTime>)> = self
            .jobs
            .values()
            .filter(|job| job.queue == queue_name && job.status == JOB_STATUS_SUBMITTED)
            .map(|job| (job.id.clone(), job.settles_at))
            .collect();
        for (id, previous_due) in waiting {
            if let Some(previous_due) = previous_due {
                self.pending.remove(&(previous_due, id.clone()));
            }
            // The index entry is written only where the job it names is: the two cannot be
            // left disagreeing even if the store changed under this walk.
            if let Some(job) = self.jobs.get_mut(&id) {
                job.settles_at = Some(due);
                self.pending.insert((due, id));
            }
        }
    }

    /// The next step of its run the clock owed the job `id` at `due` — if its queue lets
    /// the job begin at all.
    ///
    /// A job RUNS in two steps, which is what `JobStatus` describes ("a job's status can be
    /// SUBMITTED, PROGRESSING, COMPLETE, CANCELED, or ERROR") and what `Timing` reports two
    /// separate instants for ("the time ... that transcoding for the job began" and "the
    /// time ... that the transcoding job finished"):
    ///
    /// 1. it BEGINS — it leaves the backlog of its queue for the transcoder, which is
    ///    `PROGRESSING`, and that is the instant `Timing$StartTime` carries;
    /// 2. it ENDS — the run reaches a status nothing further is due from, and that is the
    ///    instant `Timing$FinishTime` carries.
    ///
    /// Each step is owed the same span after the one before it, so a caller reading between
    /// them observes the job PROGRESSING, exactly as a caller reading before the first
    /// observes it `SUBMITTED`. WHICH status the second step reaches is decided by the
    /// job's RUN ([`JobRunRecord`], made while the job is `PROGRESSING`): a run that
    /// produced the outputs the job's settings describe ends it in `COMPLETE`, and a run
    /// that failed, or a job nothing ran at all, ends it in `ERROR` with the reason.
    ///
    /// A job whose queue is `PAUSED` does not begin ("jobs in that queue won't begin"):
    /// it keeps `SUBMITTED` and is left with no due instant for that exit, and
    /// [`Self::rearm_queue`] gives it a fresh one when the queue is `ACTIVE` again. Its
    /// hops are untouched, because a job held in a queue is still waiting in one. The gate
    /// is on BEGINNING only — "jobs that are running when you pause a queue continue to run
    /// until they finish or result in an error" — so a job that has begun ends whatever its
    /// queue does afterwards.
    ///
    /// A job that BEGINS here has left its queue's backlog, so its hop destinations are
    /// owed nothing further ([`Self::retire_job_hops`]) while the finish it is now owed is
    /// indexed in its place; a job that ENDS here is owed nothing at all
    /// ([`Self::retire_job_clock`], the one statement of that rule).
    ///
    /// This is also the one place a job's status MOVES on its own, so it is where each move
    /// is recorded for publication to EventBridge
    /// ([`Self::record_job_state_change`]): a job held back by a paused queue has not
    /// moved and records nothing, and neither does a job a cancel took out of the queue
    /// before it ever began.
    pub(super) fn settle_job(&mut self, due: SystemTime, id: &str, now: SystemTime) {
        let held = self.jobs.get(id).is_some_and(|job| {
            job.status == JOB_STATUS_SUBMITTED && self.queue_is_paused(&job.queue)
        });
        // Read before the record is borrowed for writing: the span the finish is owed after
        // is the same one the start was owed after.
        let settles_after = self.job_settles_after;
        let Some(job) = self.jobs.get_mut(id) else {
            return;
        };
        if held {
            // The queue is paused, so this job does not begin: it keeps SUBMITTED
            // and nothing is due from it while the pause lasts.
            job.settles_at = None;
            return;
        }
        let begins = job.status == JOB_STATUS_SUBMITTED;
        let ends = job.status == JOB_STATUS_PROGRESSING;
        // A job whose RUN is still being made has not finished: its outputs are being
        // produced right now ([`JobRecord::run_claimed_at`]), so the finish is owed again a
        // span later rather than applied over a result that is not in yet. A claim a driver
        // abandoned is taken again by the next pass, so nothing is left running for ever.
        if ends && job.run.is_none() && job.run_claimed_at.is_some() {
            let next = now.checked_add(settles_after);
            job.settles_at = next;
            if let Some(next) = next {
                self.pending.insert((next, id.to_string()));
            }
            return;
        }
        // Where the finish lands, for the step that arms it. `checked_add` rather than `+`,
        // so an instant near the end of the representable range cannot panic — it simply
        // owes no finish.
        let mut finishes_at = None;
        if begins {
            job.status = JOB_STATUS_PROGRESSING.to_string();
            // `Timing$StartTime` is "the time ... that transcoding for the job began",
            // which is this instant and not the one the run ends at.
            job.timing.start_time = Some(due);
            finishes_at = due.checked_add(settles_after);
            job.settles_at = finishes_at;
        } else if ends {
            // The job's RUN decides the status its finish reaches. A run that produced
            // every output the job's settings describe is a job that transcoded, which is
            // `COMPLETE` and reports no failure at all.
            //
            // Otherwise the job failed, and what it reports is what was ESTABLISHED. An
            // input THIS emulator tried to read and could not is the service's own
            // input-probe failure, classified with the code MediaConvert publishes for it
            // and worded from the job's own `Settings.Inputs`; a run that failed reports
            // its own failure the same way ([`JobRunFailure`]). A job nothing ran at all —
            // an emulator with no object store to read the input from, or no runtime to run
            // the transcode on — reports the missing engine, which no MediaConvert error
            // code classifies, so none is reported rather than one invented.
            let produced = job.run.as_ref().is_some_and(|run| run.failure.is_none());
            // An input the service could not OPEN is why such a job failed, whatever else
            // the run then could not do: MediaConvert opens a job's inputs before it can
            // transcode them, so the probe's own verdict is taken first.
            let failure = job
                .unreadable_input
                .clone()
                .map(JobRunFailure::InputUnreadable)
                .or_else(|| job.run.as_ref().and_then(|run| run.failure.clone()));
            if produced {
                job.status = JOB_STATUS_COMPLETE.to_string();
                job.error_code = None;
                job.error_message = None;
            } else {
                job.status = JOB_STATUS_ERROR.to_string();
                match failure {
                    Some(failure) => {
                        job.error_code = failure.code();
                        job.error_message = Some(failure.message());
                    }
                    None => {
                        job.error_code = None;
                        job.error_message = Some(NO_TRANSCODER_MESSAGE.to_string());
                    }
                }
            }
            job.timing.finish_time = Some(due);
        }
        // The status the job moved into is published, whichever step moved it; recorded
        // from the record the write above left, with the instant the clock made the move
        // due.
        if begins || ends {
            self.record_job_state_change(id, due);
        }
        if begins {
            // The job is out of its queue's backlog, so it hops nowhere — but the clock
            // still owes it the end of its run, which the index entry just written is what
            // materialises.
            self.retire_job_hops(id);
            if let Some(finishes_at) = finishes_at {
                self.pending.insert((finishes_at, id.to_string()));
            }
        } else {
            // The run has ended, or the job had already reached a status nothing is due
            // from: either way nothing further is owed.
            self.retire_job_clock(id);
        }
    }

    /// Drop every change the clock still owed the job `id`, record fields and pending
    /// index entries together.
    ///
    /// The one statement of the rule every terminal transition follows: a job whose run is
    /// over is owed neither a step of that run ([`JobRecord::settles_at`]) nor any of its
    /// hop destinations, so both halves go together.
    pub(super) fn retire_job_clock(&mut self, id: &str) {
        self.retire_job_hops(id);
        let Some(job) = self.jobs.get_mut(id) else {
            return;
        };
        if let Some(due) = job.settles_at.take() {
            self.pending.remove(&(due, id.to_string()));
        }
    }

    /// Drop every HOP the clock still owed the job `id`, record fields and pending index
    /// entry together.
    ///
    /// Queue hopping moves a job "in the backlog of the queue that you submit your job to",
    /// so a job that has left that backlog — by beginning its run, or by reaching a
    /// terminal status — hops nowhere: its [`JobRecord::hops_at`] and the entries of
    /// `hop_destinations` from [`JobRecord::next_hop`] on are dropped with the
    /// `pending_hops` entry that mirrors them, so no such job can be left indexed for a
    /// move that will never be applied.
    pub(super) fn retire_job_hops(&mut self, id: &str) {
        let Some(job) = self.jobs.get_mut(id) else {
            return;
        };
        let hop_due = job.hops_at.take();
        job.next_hop = job.hop_destinations.len();
        if let Some(hop_due) = hop_due {
            self.pending_hops.remove(&(hop_due, id.to_string()));
        }
    }

    /// Everything the clock still owes the job `id`, read off BOTH halves of the store:
    /// the job's own record and the pending indexes that mirror it.
    ///
    /// Reporting only — it writes nothing. The two halves are stated separately on
    /// purpose: a record field and the index entry that materialises it are written
    /// together by every path that touches them, so a reader that sees them disagree has
    /// found a job indexed for a change its own record says is not due.
    ///
    /// Counting a job's index entries walks the two pending indexes, each bounded by
    /// [`MAX_JOBS`]; no request path calls this.
    pub fn job_clock(&self, id: &str) -> JobClock {
        let job = self.jobs.get(id);
        JobClock {
            settles_at: job.and_then(|job| job.settles_at),
            hops_at: job.and_then(|job| job.hops_at),
            hops_left: job.map_or(0, |job| {
                job.hop_destinations.len().saturating_sub(job.next_hop)
            }),
            pending_settles: self.pending.iter().filter(|(_, held)| held == id).count(),
            pending_hops: self
                .pending_hops
                .iter()
                .filter(|(_, held)| held == id)
                .count(),
        }
    }

    /// The completion the clock owed the jobs query `id`: it leaves the non-terminal
    /// `SUBMITTED` status for `COMPLETE`, and its results become readable.
    ///
    /// CONSTANT work per query, deliberately: the page was matched once when the query
    /// was started ([`Self::accept_jobs_query`]), so no amount of queries coming due in
    /// the same instant can make one `settle` call do more than a status flip each.
    pub(super) fn complete_jobs_query(&mut self, id: &str) {
        if let Some(query) = self.jobs_queries.get_mut(id) {
            if query.status == JOBS_QUERY_STATUS_SUBMITTED {
                query.status = JOBS_QUERY_STATUS_COMPLETE.to_string();
            }
            // A completed query is terminal, so the clock owes it nothing further.
            query.completes_at = None;
        }
    }

    /// The completion the clock owed a job's share attempt at `due`: it leaves the
    /// non-terminal `INITIATED` status for `SHARED`.
    ///
    /// CONSTANT work per attempt, as the two transitions above are: one status flip on
    /// the job the entry names, reached by a single map probe.
    ///
    /// An entry whose instant is not the one the job's CURRENT attempt is waiting for
    /// belongs to an attempt a later `CreateResourceShare` superseded, and completes
    /// nothing: the newer attempt keeps the whole span it was given.
    pub(super) fn complete_share(&mut self, due: SystemTime, id: &str) {
        if let Some(attempt) = self.jobs.get_mut(id).and_then(|job| job.share.as_mut()) {
            if attempt.completes_at != Some(due) {
                return;
            }
            if attempt.status == SHARE_STATUS_INITIATED {
                attempt.status = SHARE_STATUS_SHARED.to_string();
            }
            // A shared attempt is terminal, so the clock owes it nothing further.
            attempt.completes_at = None;
        }
    }

    /// The hop the clock owed the job `id` at `due`: it has now waited its next hop
    /// destination's `WaitMinutes` in the queue it is in, so that hop destination moves it.
    ///
    /// `HopDestination` is "Configuration for a destination queue to which the job can hop
    /// once a customer-defined minimum wait time has passed" and
    /// `CreateJob$HopDestinations` is "Use queue hopping to avoid overly long waits in the
    /// backlog of the queue that you submit your job to" — so the move is owed to a job
    /// still WAITING in its queue. A job that has already left `SUBMITTED` is in no
    /// backlog, and nothing hops it.
    ///
    /// The job moves to the queue the hop names, or to the default queue when it names
    /// none ([`hop_destination_queue_name`]), takes the hop's `Priority` when it states one
    /// ("you can specify a different relative priority for the job in the destination
    /// queue. If you don't specify, the relative priority will remain the same as in the
    /// previous queue"), and the move is recorded as a `QueueTransition`. The hop
    /// destinations are used in request order, at most once each, and the NEXT one's wait is
    /// counted from THIS move, because the wait is time the job spends in the queue it is
    /// in.
    ///
    /// CONSTANT work per hop, as the three transitions above are: one hop destination read
    /// by index, one queue probed, one transition pushed.
    ///
    /// An entry whose instant is not the one the job is waiting for belongs to a hop that
    /// has already been materialised, and moves nothing.
    pub(super) fn hop_job(&mut self, due: SystemTime, id: &str) {
        let Some(job) = self.jobs.get(id) else {
            return;
        };
        if job.hops_at != Some(due) {
            return;
        }
        let waiting = job.status == JOB_STATUS_SUBMITTED;
        let hop = job.hop_destinations.get(job.next_hop).cloned();
        let next_hop = job.next_hop + 1;
        // The wait of the hop AFTER this one, counted from the instant this one lands.
        // `checked_add` rather than `+`, so a wait near the end of the representable range
        // cannot panic — it simply owes no further hop.
        let next_due = job
            .hop_destinations
            .get(next_hop)
            .and_then(hop_wait)
            .and_then(|wait| due.checked_add(wait));
        let source = job.queue.clone();
        // A destination this store no longer holds moves nothing, because a job's `Queue`
        // never names a queue no reader can see; neither does one that resolves to the
        // queue the job is already in.
        let destination = hop
            .as_ref()
            .map(|hop| hop_destination_queue_name(hop).to_string())
            .filter(|name| name != &source && self.queues.contains_key(name));
        let priority = hop.as_ref().and_then(|hop| hop.priority);
        let destination_paused = destination
            .as_deref()
            .is_some_and(|name| self.queue_is_paused(name));
        let settles_after = self.job_settles_after;

        if !waiting {
            // The job is no longer waiting in a queue, so none of its hop destinations is
            // owed anything further — the same rule a job that BEGINS applies. Whatever
            // step of its run the clock still owes it is untouched: a running job is not
            // waiting in a backlog, and it still has to end.
            self.retire_job_hops(id);
            return;
        }
        let Some(job) = self.jobs.get_mut(id) else {
            return;
        };
        job.hops_at = None;
        job.next_hop = next_hop;
        let mut rearmed = None;
        if let Some(destination) = destination {
            job.queue_transitions.push(QueueTransitionRecord {
                source_queue: source,
                destination_queue: destination.clone(),
                at: due,
            });
            job.queue = destination;
            if let Some(priority) = priority {
                job.priority = priority;
            }
            // A job held back by a PAUSED queue carries no due instant for its exit from
            // `SUBMITTED`; once it has hopped to a queue that lets it begin, the clock owes
            // it that exit again, over the same span [`Self::rearm_queue`] gives it.
            if job.settles_at.is_none() && !destination_paused {
                let settles_at = due.checked_add(settles_after);
                job.settles_at = settles_at;
                rearmed = settles_at;
            }
        }
        job.hops_at = next_due;
        if let Some(settles_at) = rearmed {
            self.pending.insert((settles_at, id.to_string()));
        }
        if let Some(next_due) = next_due {
            self.pending_hops.insert((next_due, id.to_string()));
        }
    }
}
