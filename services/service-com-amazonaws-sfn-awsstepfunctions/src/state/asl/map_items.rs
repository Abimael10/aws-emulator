// SPDX-License-Identifier: CC0-1.0

//! How a `Map` state's ITEMS run: concurrently, up to the state's `MaxConcurrency`,
//! against the one store this emulator serves.
//!
//! "MaxConcurrency … an upper bound on how many invocations of the Iterator may run in
//! parallel": the items of one such bound RUN AT THE SAME TIME, so an iteration's own
//! control flow may depend on what its siblings have already done — a counter, a lock,
//! a ledger, a barrier — and a Distributed Map's items are full child workflow
//! executions issuing real service calls, so what one of them commits is what the next
//! one reads. The unit that runs is therefore the one whose own clock is EARLIEST: each
//! iteration is walked one state at a time ([`Interpreter::walk_one`]) and yields to its
//! siblings as soon as a `Wait` of its own (or a retry interval) puts it later than one
//! of them, which is what makes an iteration that waits on an effect its siblings commit
//! observe that effect instead of running ahead of them.
//!
//! The walk still sleeps for none of it ([`super`]): the clock each iteration carries is
//! the offset into the run at which its next state falls due, so what decides the ORDER
//! the iterations are WALKED in is that offset and never the host's own speed. Two
//! iterations due at the same instant are walked in the order their items are listed, so
//! a definition whose items never wait reads exactly as it runs.
//!
//! What IS really concurrent is their WORK, because the bound above is a bound on work
//! running in parallel and nothing else would honour it: an iteration that reaches the
//! call its `Task` state is made of hands that call over and is PUT DOWN at it
//! ([`Interpreter::perform_call`]), so the next item begins and hands its own call over
//! beside it, and a chunk of `MaxConcurrency` items has up to that many calls being
//! performed at the same instant ([`Interpreter::run_chunk`]). An iteration that is put
//! down leaves NOTHING of its turn behind — it is walked again from the state it handed
//! the call over at, with its events, its counts and its data taken back to where that
//! turn began ([`Interpreter::put_down`], [`Interpreter::take_up`]) — so what the run
//! RECORDS is what it would have recorded had each turn been walked once, and the
//! concurrency shows only in the work.
//!
//! One concern in a file of its own, reached from the walk through
//! [`Interpreter::run_map_items`]; the names stay reachable unqualified from the module
//! root and from the walk, as the other parts of this module's tree are.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::time::Duration;

use serde_json::Value;

use super::*;

/// Where a walk of a state list has got to: the state it runs next, the document that
/// state is entered with, and the offset into the run at which it begins.
///
/// It is the whole of an iteration's position in its own state machine, which is what
/// lets one be put down between states and taken up again ([`Interpreter::walk_one`]).
pub(super) struct Cursor {
    pub(super) name: String,
    pub(super) value: Value,
    pub(super) at: Duration,
}

/// Where one item's turn BEGAN: what a turn of an iteration is taken back to when it is
/// PUT DOWN at the call it handed over ([`Interpreter::put_down`],
/// [`Interpreter::take_up`]) — the item's own position and data, and where the walk's own
/// lists and counts stood when the turn began.
struct Putdown {
    cursor: Cursor,
    retry_count: u64,
    variables: BTreeMap<String, Value>,
    assigned: BTreeMap<String, String>,
    base_context: Value,
    /// How many history events the walk had recorded, and how many of them this
    /// iteration's unit had claimed: the turn's own events sit at the end of both, so both
    /// are cut back to these.
    steps: usize,
    recorded: usize,
    /// The attempts the state being run had spent as the turn began.
    attempts_spent: Vec<u64>,
    /// What the execution being recorded for had left of its own history allowance, and
    /// the figures of the CHILD execution's history for an item that runs as one
    /// ([`ChildHistory`]): a turn's events are counted once however many times it is
    /// walked, whichever history they belong to, and that allowance is also what bounds
    /// the transitions one run makes.
    events_left: usize,
    history: Option<ChildHistory>,
    /// What was left of the interpretation work this run may do, so the turn is charged
    /// for once however many times it is walked ([`work_left`]).
    work_left: u64,
}

/// One iteration of a `Map` state that is RUNNING: where its own walk has got to, and
/// the interpreter state that belongs to it alone.
struct Iteration {
    /// The item's position among the state's items: what every event of this iteration
    /// reports, and what orders two iterations due at the same instant.
    index: usize,
    /// Where this iteration's own walk has got to.
    cursor: Cursor,
    /// The `Retry` attempt the state it is in is on, the variables its states have
    /// assigned and what the state it is in has assigned: a nested state machine owns
    /// each of them, so they are held here while a sibling runs and put back when this
    /// iteration does.
    retry_count: u64,
    variables: BTreeMap<String, Value>,
    assigned: BTreeMap<String, String>,
    /// The context object (`$$`) root this iteration's states read: the CHILD workflow
    /// execution's for a Distributed Map's item, the enclosing walk's for an inline
    /// `Map`'s.
    base_context: Value,
    /// `$$.Map.Item` for this iteration — the item and its position.
    scope: Scope,
    /// The child workflow execution this item runs as, for a Distributed Map: its name
    /// and its own ARN, taken by the record the iteration's outcome is reported through.
    child: Option<(String, String)>,
    /// "The string that contains the JSON input data" of that execution.
    child_input: Option<String>,
    /// "The total number of items processed in a child workflow execution".
    item_count: i32,
    /// How far into the run this iteration began.
    started_at: Duration,
    /// Which [`Unit`] of the state's concurrent work this iteration's events belong to.
    unit: usize,
    /// The integration call this iteration has already STARTED and not taken the answer
    /// of: the work of one item, in flight while its siblings' own work is started
    /// beside it. `None` for an iteration with no work outstanding, which is every
    /// iteration of a state whose items do not run together
    /// ([`Interpreter::items_run_together`]).
    started: Option<super::super::integration::InFlight>,
    /// The history this iteration records into when it is a CHILD workflow execution's
    /// own, for a Distributed Map's item; `None` for an inline `Map`'s iteration, whose
    /// events are steps of the execution around it ([`ChildHistory`]).
    history: Option<ChildHistory>,
}

/// The history ONE child workflow execution of a Distributed `Map` records, and the
/// allowance it records it against.
///
/// "Step Functions starts a child workflow execution for each item": the states of one
/// item are steps of THAT execution, so what they record is its history and not the
/// parent's — which is why a Distributed Map's parent neither numbers nor keeps an
/// event per item, and why the events one item records are bounded by its own
/// execution's quota ([`MAX_HISTORY_EVENTS`]) rather than by what the parent has left.
///
/// What a child records is COUNTED and not HELD ([`Interpreter::unheld`]): the store
/// records that execution's own start and outcome and nothing reports the steps between
/// them, so holding them would make what one `Map` state's concurrent items keep in
/// memory grow with the events they record. Two figures are all an iteration carries
/// between its slices.
///
/// `Copy`, because a turn that is PUT DOWN is taken back to the figures its own history
/// stood at ([`Putdown`]): what a turn recorded is counted once however many times it is
/// walked.
#[derive(Clone, Copy)]
pub(super) struct ChildHistory {
    /// What is left of that execution's own allowance: a child whose `Next` chain forms
    /// a cycle is stopped by its OWN history, with the same failure its parent would
    /// end on ([`history_limit_exceeded`]).
    events_left: usize,
    /// The id the first event this child records carries: its history is its own,
    /// numbered from the `ExecutionStarted` its record opens with.
    first_event_id: i64,
    /// How many events it has recorded, which is what numbers the next one
    /// ([`Interpreter::next_event_id`]).
    numbered: usize,
}

/// The events a child workflow execution's record carries besides the ones its states
/// record: the `ExecutionStarted` it opens with and the terminal event it ends on.
const CHILD_BRACKET_EVENTS: usize = 2;

/// What the items of ONE `Map` attempt have done so far, as they are run.
struct Tallied {
    /// What the iterations produced, each with the position of its item: "the output is
    /// a JSON array, where each element is the result of an iteration", in the order
    /// the ITEMS are listed, while the iterations themselves finish in whatever order
    /// their own clocks decide.
    results: Vec<(usize, Value)>,
    /// How many bytes those results hold, which is bounded as the array GROWS.
    results_bytes: usize,
    /// How many iterations FAILED, which is what the state's failure budget counts.
    failures: u64,
    /// The iteration that failed FIRST, with the offset it failed at: the failure an
    /// intolerant state reports, and the instant its other iterations stopped.
    first_failure: Option<(Failure, Duration)>,
    /// Each item that finished, with the offset it finished at.
    items: Vec<MapItemOutcome>,
    /// The child workflow execution each item of a DISTRIBUTED attempt ran as, with the
    /// position of its item: the iterations finish in whatever order their own clocks
    /// decide, and what a Map Run reports is the order they were STARTED in.
    children: Vec<(usize, StartedChildExecution)>,
    /// Where each iteration's own events sit in this walk's list, so an iteration the
    /// state's failure aborted can be cut back to the instant it failed.
    units: Vec<Unit>,
    /// Which of the child executions the attempt being CONTINUED ran have been claimed
    /// by an item of this one, so two items never keep the same execution
    /// ([`Interpreter::kept_child`]).
    claimed: Vec<bool>,
    /// How many items of this attempt have been started, which is the next one's own
    /// position among the state's items.
    started: usize,
}

/// What starting one item left to be scheduled.
enum Started {
    /// The item's iteration is RUNNING, and is scheduled by the clock it carries.
    Running(Box<Iteration>),
    /// Nothing: the item's outcome is already recorded — a child workflow execution an
    /// attempt being CONTINUED already succeeded at, whose result stands as this
    /// attempt's.
    Recorded,
}

/// The results of one attempt's iterations as the state puts them out: "the output is a
/// JSON array, where each element is the result of an iteration" — in the order the
/// ITEMS are listed, whatever order the iterations finished in.
fn in_item_order(results: &mut Vec<(usize, Value)>) -> Value {
    let mut produced = std::mem::take(results);
    produced.sort_by_key(|(index, _)| *index);
    Value::Array(produced.into_iter().map(|(_, value)| value).collect())
}

impl Interpreter<'_> {
    /// [`Interpreter::run_map_items`]' own work, inside the stop it restores.
    pub(super) fn walk_map_items(
        &mut self,
        plan: &MapPlan<'_>,
        effective: &Value,
        context: &Value,
        at: Duration,
        tally: &mut MapTally,
    ) -> Attempt {
        *tally = MapTally::default();
        let mut tallied = Tallied {
            results: Vec::new(),
            results_bytes: 0,
            failures: 0,
            first_failure: None,
            items: Vec::new(),
            children: Vec::new(),
            units: Vec::new(),
            claimed: vec![false; plan.resumed.map_or(0, <[_]>::len)],
            started: 0,
        };
        let attempt = self.run_item_chunks(plan, effective, context, at, &mut tallied);
        // What this attempt's items did, however it ended: the counts belong to the
        // attempt rather than to its result, so they are reported by every path out.
        tally.items = std::mem::take(&mut tallied.items);
        let mut children = std::mem::take(&mut tallied.children);
        children.sort_by_key(|(index, _)| *index);
        tally.children = children.into_iter().map(|(_, child)| child).collect();
        attempt
    }

    /// Every item of the state, chunk by chunk: `MaxConcurrency` of them run at once,
    /// and a chunk's span is its longest item's rather than the sum.
    ///
    /// A state that declares no `MaxConcurrency` asks for every item at once, and one
    /// that declares a figure asks for that many: either way no more than
    /// [`MAX_LIVE_MAP_ITERATIONS`] of them are LIVE together, so what the fan-out holds
    /// in memory does not grow with the item count. The items past it are started as
    /// the live ones finish, so every item still runs.
    fn run_item_chunks(
        &mut self,
        plan: &MapPlan<'_>,
        effective: &Value,
        context: &Value,
        at: Duration,
        tallied: &mut Tallied,
    ) -> Attempt {
        let batch = match plan.concurrency {
            0 => plan.items.len().clamp(1, MAX_LIVE_MAP_ITERATIONS),
            declared => declared.min(MAX_LIVE_MAP_ITERATIONS),
        };
        // "ToleratedFailureCount" and "ToleratedFailurePercentage" are what lets a
        // state go on iterating past a failed item; a state that declares neither
        // tolerates none, so the first failure is the state's own and the iterations
        // that had not finished stop there.
        let tolerant = plan.distributed || plan.tolerance_declared;
        let mut at = at;
        for chunk in plan.items.chunks(batch) {
            match self.run_chunk(plan, chunk, effective, context, at, tallied, tolerant) {
                Ok(end) => at = end,
                Err(attempt) => return attempt,
            }
            // A state that tolerates no failure has already failed, so the items it
            // had not started yet are never started.
            if tallied.failures > 0 && !tolerant {
                break;
            }
        }

        if tallied.failures == 0 {
            return (Ok(in_item_order(&mut tallied.results)), at);
        }
        if super::super::within_tolerated_failures(
            i64::try_from(tallied.failures).unwrap_or(i64::MAX),
            i64::try_from(plan.items.len()).unwrap_or(i64::MAX),
            i64::try_from(plan.tolerated_count).unwrap_or(i64::MAX),
            plan.tolerated_percentage,
        ) {
            return (Ok(in_item_order(&mut tallied.results)), at);
        }
        // A Distributed Map reports its own tolerated-failure error; an inline Map
        // has no threshold to exceed, so the iteration's own failure is the state's.
        // What terminated the state is the BREACH of the threshold rather than any one
        // item's error, which is what the service's own sentence for it says
        // ([`TOLERATED_FAILURE_CAUSE`]).
        if tolerant {
            return (
                Err(named(
                    TOLERATED_FAILURE_ERROR,
                    TOLERATED_FAILURE_CAUSE.to_string(),
                )),
                at,
            );
        }
        let Some((failure, when)) = tallied.first_failure.take() else {
            return (Err(runtime("a 'Map' iteration failed".to_string())), at);
        };
        self.abort_units(when, &tallied.units);
        (Err(failure), when)
    }

    /// Run ONE chunk of the state's items — the `MaxConcurrency` of them that run at
    /// once — and report the offset at which the last of them finished.
    ///
    /// The items of a chunk all BEGIN at `batch_start` and run concurrently, so the
    /// iteration that runs is the one whose own clock is earliest, by the position of
    /// its item where two are due at the same instant: an iteration is walked until a
    /// `Wait` of its own puts it later than a sibling, and the sibling then runs. An
    /// item that has not begun yet begins at `batch_start`, which is no later than any
    /// live iteration's clock, so it is started as soon as every live one has moved past
    /// that instant — which is how a chunk whose items never wait runs them in the order
    /// they are listed, one after another.
    ///
    /// Their WORK, though, is in flight together: an iteration that reaches the call its
    /// `Task` state is made of hands that call over and is PUT DOWN there
    /// ([`Interpreter::perform_call`]), so the next item begins and hands its own call
    /// over beside it, and an iteration waiting for an answer is taken up again as soon
    /// as no sibling due at or before it has work left to start. A chunk of
    /// `MaxConcurrency` items therefore has up to that many calls really being made at
    /// the same time, which is what the member bounds
    /// ([`super::super::integration::Tasks::calls_in_flight`] bounds how many of them
    /// this walk runs beside each other). An iteration that is put down leaves NOTHING of
    /// its turn behind ([`Interpreter::take_up`]): it is walked again from the state it
    /// handed the call over at, so the events the chunk records, the counts it spends and
    /// the data its items carry are what they would have been had each turn been walked
    /// once.
    #[allow(clippy::too_many_arguments)]
    fn run_chunk(
        &mut self,
        plan: &MapPlan<'_>,
        chunk: &[Value],
        effective: &Value,
        context: &Value,
        batch_start: Duration,
        tallied: &mut Tallied,
        tolerant: bool,
    ) -> Result<Duration, Attempt> {
        let mut batch_end = batch_start;
        // Whether the work of this chunk's items may be in flight together: a chunk of
        // one item has nothing to run beside, and what else decides it is the walk
        // itself.
        let together = chunk.len() > 1 && self.items_run_together();
        // Every iteration of this chunk that is still RUNNING, by the slot the schedule
        // names it by: a slot keeps its place so the schedule goes on naming the others,
        // and what an iteration that has ended carried is dropped there and then.
        let mut running: Vec<Option<Iteration>> = Vec::new();
        // Which iteration runs next: the live one due earliest, by the position of its
        // item where two are due together.
        let mut ready: BinaryHeap<Reverse<(Duration, usize, usize)>> = BinaryHeap::new();
        // The iterations whose own work is IN FLIGHT, ordered the same way: each was
        // walked up to the call it handed over, and is taken up again — to take that
        // call's answer — once there is no sibling left to start work for.
        let mut in_flight: BinaryHeap<Reverse<(Duration, usize, usize)>> = BinaryHeap::new();
        let mut begun = 0usize;
        loop {
            let begins = begun < chunk.len()
                && ready
                    .peek()
                    .is_none_or(|Reverse((due, _, _))| *due > batch_start);
            if let Some(item) = begins.then(|| chunk.get(begun)).flatten() {
                begun += 1;
                match self.start_item(plan, item, effective, context, batch_start, tallied) {
                    Ok(Started::Running(iteration)) => {
                        let slot = running.len();
                        ready.push(Reverse((iteration.cursor.at, iteration.index, slot)));
                        running.push(Some(*iteration));
                    }
                    Ok(Started::Recorded) => {}
                    Err(failure) => return Err((Err(failure), batch_end)),
                }
                continue;
            }
            // An iteration with no work outstanding runs before one that is waiting for
            // its own where the two are due TOGETHER: that is what gets the work of as
            // many of this chunk's items as may run together started before any of their
            // answers is waited for. An iteration due EARLIER still runs first, whichever
            // queue it sits in, so the order the iterations are walked in is the clock's
            // as it has always been.
            let next_ready = ready.peek().map(|Reverse((due, _, _))| *due);
            let next_in_flight = in_flight.peek().map(|Reverse((due, _, _))| *due);
            let takes_ready = match (next_ready, next_in_flight) {
                (Some(ready_at), Some(flight_at)) => ready_at <= flight_at,
                (ready_at, _) => ready_at.is_some(),
            };
            let turn = match takes_ready {
                true => ready.pop().map(|next| (next, true)),
                false => in_flight.pop().map(|next| (next, false)),
            };
            let Some((Reverse((_, _, slot)), has_no_work_outstanding)) = turn else {
                break;
            };
            let Some(unit) = running
                .get(slot)
                .and_then(Option::as_ref)
                .map(|iteration| iteration.unit)
            else {
                break;
            };
            let ended = {
                let Some(iteration) = running.get_mut(slot).and_then(Option::as_mut) else {
                    break;
                };
                let Some(recorded) = tallied.units.get_mut(unit).map(|unit| &mut unit.steps) else {
                    break;
                };
                // This turn may hand its call over and be put down when the items run
                // together, it has no work outstanding already, and this walk has room
                // for another call in flight.
                let may_be_put_down = has_no_work_outstanding
                    && together
                    && in_flight.len() < self.tasks.calls_in_flight();
                let putdown = may_be_put_down.then(|| self.put_down(iteration, recorded));
                let ended = self.run_slice(plan, iteration, recorded, may_be_put_down);
                if self.unwinding {
                    self.unwinding = false;
                    // The turn handed its work over and was put down: NOTHING of it is
                    // kept, and the iteration waits at the state that handed the call
                    // over until there is no sibling left to start work for.
                    //
                    // Only a turn that MAY be put down hands a call over, so there is
                    // always something to take it back to; a turn that somehow unwound
                    // without one cannot be taken back, and nothing of this attempt is
                    // reported as having run rather than its items being left half-run.
                    let Some(putdown) = putdown else {
                        return Err((
                            Err(runtime(format!(
                                "the work of an item of state '{}' was handed over by a turn \
                                 that cannot be walked again, so nothing was run for it",
                                plan.name
                            ))),
                            batch_end,
                        ));
                    };
                    self.take_up(iteration, recorded, putdown);
                    in_flight.push(Reverse((iteration.cursor.at, iteration.index, slot)));
                    continue;
                }
                ended
            };
            let Some(branch) = ended else {
                // The iteration yielded: its own clock has moved past a sibling's, so
                // the sibling runs and this one is taken up again at the instant it is
                // due.
                if let Some(iteration) = running.get(slot).and_then(Option::as_ref) {
                    ready.push(Reverse((iteration.cursor.at, iteration.index, slot)));
                }
                continue;
            };
            batch_end = batch_end.max(branch.end);
            // The iteration has ENDED: what it carried is taken out of its slot here,
            // and nothing schedules it again.
            let Some(mut iteration) = running.get_mut(slot).and_then(Option::take) else {
                break;
            };
            Self::item_done(&mut iteration, &branch, tallied);
            let index = iteration.index;
            let from = self.steps.len();
            // The event the enclosing execution records for an iteration it stepped
            // through; a DISTRIBUTED item's outcome is its CHILD execution's own, which
            // the Map Run reports rather than the parent's history ([`ChildHistory`]).
            if iteration.history.is_none() {
                let outcome_event = match &branch.outcome {
                    Ok(_) => super::super::EVENT_MAP_ITERATION_SUCCEEDED,
                    Err(_) => super::super::EVENT_MAP_ITERATION_FAILED,
                };
                self.push_detailed(outcome_event, plan.name, branch.end, iteration_details(index));
            }
            if let Some(unit) = tallied.units.get_mut(unit) {
                unit.steps.extend(from..self.steps.len());
            }
            match branch.outcome {
                Ok(value) => {
                    // The array the iterations are building is a document this state
                    // will put out, so it is bounded as it grows rather than only once
                    // it is whole.
                    match measure_payload(&value, UNNAMED_PRODUCER) {
                        Ok(bytes) => {
                            tallied.results_bytes = tallied.results_bytes.saturating_add(bytes);
                        }
                        Err(failure) => return Err((Err(failure), batch_end)),
                    }
                    // The array the iterations build IS this state's result, so it
                    // outgrowing the limit is the same quota failure a single iteration
                    // past it raises, worded the one way the service words it: the state
                    // being executed is what names itself in it ([`result_too_large`],
                    // named by [`diagnosed`] or by the catcher that takes it over).
                    if tallied.results_bytes > MAX_PAYLOAD_BYTES {
                        return Err((
                            Err(named(DATA_LIMIT_ERROR, result_too_large(UNNAMED_PRODUCER))),
                            batch_end,
                        ));
                    }
                    tallied.results.push((index, value));
                    tallied.items.push(MapItemOutcome {
                        finished_at: branch.end,
                        failed: false,
                    });
                }
                Err(failure) => {
                    tallied.failures += 1;
                    tallied.items.push(MapItemOutcome {
                        finished_at: branch.end,
                        failed: true,
                    });
                    if tallied
                        .first_failure
                        .as_ref()
                        .is_none_or(|(_, when)| branch.end < *when)
                    {
                        tallied.first_failure = Some((failure, branch.end));
                    }
                    // A state that tolerates no failure has already failed, so the
                    // iterations running BESIDE this one in the same chunk are stopped
                    // at the instant it failed, exactly as the items it had not started
                    // yet are never started
                    // ([`Interpreter::stopped_by_a_sibling`]).
                    if !tolerant {
                        let stop = tallied
                            .first_failure
                            .as_ref()
                            .map_or(branch.end, |(_, when)| *when);
                        self.cancel_after =
                            Some(self.cancel_after.map_or(stop, |enclosing| enclosing.min(stop)));
                    }
                }
            }
        }
        Ok(batch_end)
    }

    /// Start ONE item's iteration at `batch_start`: the document it is handed, the child
    /// workflow execution it runs as for a Distributed Map, and the event that records
    /// it as started.
    ///
    /// The allowance the enclosing execution has left is read BEFORE the work this item
    /// needs is done, so the per-item work (the context the iteration reads, the
    /// `ItemSelector` it is given) stops with the history the execution may still record
    /// rather than being done for every remaining item of a long array.
    ///
    /// A DISTRIBUTED item is handed an allowance of its OWN ([`ChildHistory`]): it runs
    /// as a child workflow execution, whose events and whose quota are that execution's,
    /// so neither grows with how many items the state fans out over — which is what the
    /// mode is for.
    fn start_item(
        &mut self,
        plan: &MapPlan<'_>,
        item: &Value,
        effective: &Value,
        context: &Value,
        batch_start: Duration,
        tallied: &mut Tallied,
    ) -> Result<Started, Failure> {
        if let Some(failure) = self.out_of_work() {
            return Err(failure);
        }
        if self.events_left == 0 {
            return Err(history_limit_exceeded());
        }
        let index = tallied.started;
        tallied.started += 1;
        // `$$.Map.Item` is what the iteration reads the item and its position through,
        // both in the `ItemSelector` evaluated here and in every state of the iteration.
        let map_item = serde_json::json!({ "Index": index, "Value": item });
        let item_context = with_map_item(context, &map_item);
        let scope = Scope {
            map_item: Some(map_item),
        };
        let item_input = match plan.selector {
            Some(template) => {
                self.render_selector(plan.language, template, effective, &item_context)?
            }
            None => item.clone(),
        };
        // "Step Functions preserves the results and execution history of the successful
        // steps, and doesn't rerun these steps when you redrive an execution": an item
        // whose child workflow execution SUCCEEDED in the attempt being continued is not
        // run a second time. Its result stands as this attempt's, the execution it ran
        // as is reported unchanged — and so is left untouched by the store — and the
        // per-item work it already did is therefore not repeated.
        let document = item_input.to_string();
        if let Some((position, kept)) = self.kept_child(plan, index, &document, &tallied.claimed) {
            if let Some(claimed) = tallied.claimed.get_mut(position) {
                *claimed = true;
            }
            let value = kept
                .output
                .as_deref()
                .and_then(|output| serde_json::from_str(output).ok())
                .unwrap_or(Value::Null);
            let bytes = measure_payload(&value, UNNAMED_PRODUCER)?;
            tallied.results_bytes = tallied.results_bytes.saturating_add(bytes);
            tallied.results.push((index, value));
            tallied.items.push(MapItemOutcome {
                finished_at: batch_start,
                failed: false,
            });
            tallied.children.push((
                index,
                StartedChildExecution {
                    name: kept.name.clone(),
                    arn: kept.arn.clone(),
                    input: kept.input.clone(),
                    output: kept.output.clone(),
                    error: None,
                    cause: None,
                    status: super::super::EXECUTION_STATUS_SUCCEEDED,
                    started_at: batch_start,
                    finished_at: batch_start,
                    item_count: kept.item_count,
                    // The dates the execution it ran as already reports, which do not
                    // move by being kept.
                    kept: Some((kept.start_date, kept.stop_date)),
                },
            ));
            return Ok(Started::Recorded);
        }
        // A DISTRIBUTED Map "starts a child workflow execution for each item", so this
        // item's iteration runs as an execution of its own: its ARN is minted HERE,
        // before the iteration, because the item's states read it through
        // `$$.Execution.Id` — and the context they read is the CHILD's, not the
        // parent's. An inline Map starts no execution, so its iteration keeps the
        // parent's context.
        let child = plan.distributed.then(|| {
            let name = self.fresh_child_name();
            let arn = super::super::execution_arn(
                &self.region,
                &format!("{}/{}", self.machine_name, plan.label),
                &name,
            );
            (name, arn)
        });
        let base_context = match &child {
            Some((name, arn)) => self.child_context(arn, name, &item_input, batch_start),
            None => self.base_context.clone(),
        };
        // The child's own `input` is the document this iteration was handed, kept before
        // the walk takes it: "the string that contains the JSON input data" of that
        // execution.
        let child_input = child.as_ref().map(|_| document);
        // The HISTORY this iteration records into: an inline `Map`'s iteration is a step
        // of the execution around it, so its events are that execution's and are
        // recorded here; a DISTRIBUTED item runs as a child workflow execution, so its
        // events are the CHILD's, numbered from that execution's own start and bounded
        // by that execution's own quota ([`ChildHistory`]).
        let history = child.as_ref().map(|_| ChildHistory {
            events_left: MAX_HISTORY_EVENTS.saturating_sub(CHILD_BRACKET_EVENTS),
            first_event_id: FIRST_EVENT_ID + 1,
            numbered: 0,
        });
        // One iteration per item: an INLINE one is STARTED here, carrying the index of
        // the item it was handed, and records its own outcome once it ends — which is how
        // a caller observes the fan-out, and how many items ran at once. A DISTRIBUTED
        // state's own run is what the parent records: the iterations are child
        // executions, which the parent's history does not step through.
        let from = self.steps.len();
        if history.is_none() {
            self.push_detailed(
                super::super::EVENT_MAP_ITERATION_STARTED,
                plan.name,
                batch_start,
                iteration_details(index),
            );
        }
        let unit = tallied.units.len();
        tallied.units.push(Unit {
            steps: (from..self.steps.len()).collect(),
            map_runs_from: self.map_runs.len(),
            // Only an iteration the enclosing execution's history steps through has an
            // event of its own to be cut off at.
            aborted: history.as_ref().map_or_else(
                || {
                    Some(Step {
                        event_type: super::super::EVENT_MAP_ITERATION_ABORTED,
                        state_name: Some(plan.name.to_string()),
                        after: Duration::ZERO,
                        terminal: false,
                        details: iteration_details(index),
                    })
                },
                |_| None,
            ),
        });
        Ok(Started::Running(Box::new(Iteration {
            index,
            cursor: Cursor {
                name: plan.start_at.to_string(),
                value: item_input,
                at: batch_start,
            },
            // A nested machine's states carry their OWN retry counts, and their own
            // VARIABLES: its states read what reaching the state that declares it
            // assigned, and what they assign does not outlive the machine, which is how
            // the language scopes an `Assign`.
            retry_count: 0,
            variables: self.variables.clone(),
            assigned: BTreeMap::new(),
            base_context,
            scope,
            child,
            child_input,
            // One item per child, or the batch's own count for a state that declares an
            // `ItemBatcher`, whose children are each handed one batch: see
            // [`StartedChildExecution::item_count`].
            item_count: match plan.batched {
                true => i32::try_from(super::super::item_batcher::item_count(item))
                    .unwrap_or(i32::MAX),
                false => 1,
            },
            started_at: batch_start,
            unit,
            // Nothing is in flight for an iteration that has not been walked yet.
            started: None,
            history,
        })))
    }

    /// Run ONE iteration until its own clock ADVANCES, and report what it ended as:
    /// `None` for an iteration that is still running, which has yielded to a sibling due
    /// before it.
    ///
    /// The interpreter state the iteration owns is swapped in while it runs and taken
    /// back when it stops, so no iteration reads another's `Retry` attempt, variables or
    /// context object — and whatever the `Map` state itself runs in is put back by every
    /// path out.
    ///
    /// `recorded` collects where this slice's own history events sit in the walk's list,
    /// so an iteration the state's failure aborts can be cut back to the instant it
    /// failed however its events are interleaved with its siblings'
    /// ([`Interpreter::abort_units`]). A DISTRIBUTED item records into its own
    /// execution's history instead ([`ChildHistory`]), which is swapped in for as long
    /// as the slice runs — so the events of one item, their numbering and the quota they
    /// are held to are the CHILD execution's, none of them is kept, and nothing of them
    /// sits in the parent's list for a cut to name.
    ///
    /// `may_be_put_down` is whether this turn may hand the first call it reaches over and
    /// stop there, leaving it in flight while a sibling's own work is started: the walk
    /// then reports it UNWOUND ([`Interpreter::unwinding`]), and the caller takes the turn
    /// back to where it began.
    fn run_slice(
        &mut self,
        plan: &MapPlan<'_>,
        iteration: &mut Iteration,
        recorded: &mut Vec<usize>,
        may_be_put_down: bool,
    ) -> Option<Branch> {
        // One level deeper for as long as this slice runs, exactly as a walk that runs
        // the iteration to its end in one go is: the nesting a definition may ask for is
        // capped the same way, and a run-a-job task inside an iteration is refused by
        // the same count.
        if let Some(failure) = self.too_deeply_nested() {
            return Some(Branch {
                outcome: Err(failure),
                end: iteration.cursor.at,
                suspended: false,
            });
        }
        let began = iteration.cursor.at;
        let enclosing_retry = std::mem::replace(&mut self.retry_count, iteration.retry_count);
        let enclosing_variables =
            std::mem::replace(&mut self.variables, std::mem::take(&mut iteration.variables));
        let enclosing_assigned =
            std::mem::replace(&mut self.assigned, std::mem::take(&mut iteration.assigned));
        let enclosing_context = std::mem::replace(
            &mut self.base_context,
            std::mem::take(&mut iteration.base_context),
        );
        // The work this iteration already has in flight, and whether it may put another
        // one there: both belong to the iteration that is running, so they are swapped in
        // with the rest of what it owns. The reading the sample count is taken at is THIS
        // turn's beginning, which is what says whether walking it again would ask for the
        // same call ([`Interpreter::perform_call`]).
        let enclosing_started = std::mem::replace(&mut self.started, iteration.started.take());
        let enclosing_probing = std::mem::replace(
            &mut self.probing,
            may_be_put_down.then(super::super::samples_drawn),
        );
        // The child execution's own history, for an item that runs as one: its
        // allowance, its numbering and the count of what it has recorded are swapped in
        // here and taken back below, exactly as the interpreter state above is. Its
        // events are not HELD, so the enclosing execution's own list is left exactly as
        // it stands and nothing of the item's steps is added to it.
        let enclosing_history = iteration.history.as_mut().map(|history| {
            (
                self.unheld.replace(history.numbered),
                std::mem::replace(&mut self.events_left, history.events_left),
                std::mem::replace(&mut self.first_event_id, history.first_event_id),
            )
        });
        self.depth += 1;
        self.map_depth += 1;
        let from = self.steps.len();
        let ended = loop {
            match self.walk_one(plan.states, &mut iteration.cursor, &iteration.scope) {
                Some(branch) => break Some(branch),
                None if iteration.cursor.at != began => break None,
                None => {}
            }
        };
        self.map_depth -= 1;
        self.depth -= 1;
        match (iteration.history.as_mut(), enclosing_history) {
            (Some(history), Some((unheld, events_left, first_event_id))) => {
                history.numbered = std::mem::replace(&mut self.unheld, unheld)
                    .unwrap_or(history.numbered);
                history.events_left = std::mem::replace(&mut self.events_left, events_left);
                self.first_event_id = first_event_id;
            }
            // An inline iteration's events are the enclosing execution's, so where they
            // sit in its list is what a cut of this unit names.
            _ => recorded.extend(from..self.steps.len()),
        }
        iteration.retry_count = std::mem::replace(&mut self.retry_count, enclosing_retry);
        iteration.variables = std::mem::replace(&mut self.variables, enclosing_variables);
        iteration.assigned = std::mem::replace(&mut self.assigned, enclosing_assigned);
        iteration.base_context = std::mem::replace(&mut self.base_context, enclosing_context);
        iteration.started = std::mem::replace(&mut self.started, enclosing_started);
        self.probing = enclosing_probing;
        ended
    }

    /// Whether the items of the `Map` state being run may have their work IN FLIGHT
    /// TOGETHER, which is what makes `MaxConcurrency` a number of items whose work is
    /// really being done at the same time.
    ///
    /// Two kinds of walk run them one at a time instead. The items of a NESTED `Map` do:
    /// the item of the state around them is itself a turn that may be put down, and one
    /// turn is put down at a time, so it is the outermost state whose items run together.
    /// And so do the items of a walk that CONTINUES an execution — from a worker's reply,
    /// from a job that ended, from an answered token, or from a redrive — because what
    /// such a walk applies to the state it continues at is TAKEN as that state runs, and
    /// a turn that is put down and walked again would have to apply it twice.
    fn items_run_together(&self) -> bool {
        self.map_depth == 0
            && self.reply.is_none()
            && self.job_reply.is_none()
            && self.callback_reply.is_none()
            && self.resumed.is_none()
            && self.resumed_attempts.is_none()
            && self.continued_entry.is_none()
            && self.resume_frames.is_empty()
            && !self.skipping_entry
            && self.waiting.is_none()
            && self.parked.is_none()
    }

    /// Where one item's turn BEGAN, as everything that turn may change stood then: what
    /// a turn that handed its call over and was put down is taken back to
    /// ([`Interpreter::take_up`]).
    ///
    /// A turn is put down only at the FIRST call it reaches, and only when nothing it has
    /// already done can be undone by walking it again: it has performed no call of its
    /// own ([`Interpreter::perform_call`]), entered no `Map` or `Parallel` state
    /// ([`Interpreter::nested_walk`], [`Interpreter::map_state`]) and sampled no value
    /// ([`super::super::samples_drawn`]). So what it may have changed is the data the
    /// item carries, the history events of the states it walked, and the counts those
    /// states spent.
    ///
    /// The item's own data is COPIED here, which is what a turn that may be put down
    /// costs: one copy of the document it carries and of the variables in scope, per
    /// turn, taken only while the items run together.
    fn put_down(&self, iteration: &Iteration, recorded: &[usize]) -> Putdown {
        Putdown {
            cursor: Cursor {
                name: iteration.cursor.name.clone(),
                value: iteration.cursor.value.clone(),
                at: iteration.cursor.at,
            },
            retry_count: iteration.retry_count,
            variables: iteration.variables.clone(),
            assigned: iteration.assigned.clone(),
            base_context: iteration.base_context.clone(),
            steps: self.steps.len(),
            recorded: recorded.len(),
            attempts_spent: self.attempts_spent.clone(),
            events_left: self.events_left,
            history: iteration.history,
            work_left: work_left(),
        }
    }

    /// Take `iteration` back to where the turn that was put down began, so the item is
    /// walked again from the state it handed its call over at.
    ///
    /// Everything that turn did is undone: the history events it recorded are dropped —
    /// or, for an item that records into a child execution's own history, uncounted — the
    /// counts it spent are given back, and the data it carries is the data it was handed.
    /// So the turn costs the run nothing, and what the run reports is what it would have
    /// reported had the turn been walked once — the only difference being that the work
    /// the call asks for is now being done while a sibling's is started.
    fn take_up(&mut self, iteration: &mut Iteration, recorded: &mut Vec<usize>, from: Putdown) {
        iteration.cursor = from.cursor;
        iteration.retry_count = from.retry_count;
        iteration.variables = from.variables;
        iteration.assigned = from.assigned;
        iteration.base_context = from.base_context;
        iteration.history = from.history;
        self.steps.truncate(from.steps);
        recorded.truncate(from.recorded);
        self.attempts_spent = from.attempts_spent;
        self.events_left = from.events_left;
        put_work_back(from.work_left);
    }

    /// Record the child workflow execution ONE item ran as, as it ended: an item that
    /// produced a result is an execution that SUCCEEDED with that result as its output,
    /// and one whose iteration failed is an execution that FAILED with that failure's
    /// own error and cause. An inline `Map` starts none, so there is nothing to record.
    fn item_done(iteration: &mut Iteration, branch: &Branch, tallied: &mut Tallied) {
        let Some((name, arn)) = iteration.child.take() else {
            return;
        };
        let (status, output, error, cause) = match &branch.outcome {
            Ok(value) => (
                super::super::EXECUTION_STATUS_SUCCEEDED,
                Some(value.to_string()),
                None,
                None,
            ),
            Err(failure) => (
                super::super::EXECUTION_STATUS_FAILED,
                None,
                failure.error.clone(),
                failure.cause.clone(),
            ),
        };
        tallied.children.push((
            iteration.index,
            StartedChildExecution {
                name,
                arn,
                input: iteration.child_input.take().unwrap_or_default(),
                output,
                error,
                cause,
                status,
                started_at: iteration.started_at,
                finished_at: branch.end,
                item_count: iteration.item_count,
                // This item RAN in this attempt, so the execution it ran as is one this
                // attempt started.
                kept: None,
            },
        ));
    }
}
