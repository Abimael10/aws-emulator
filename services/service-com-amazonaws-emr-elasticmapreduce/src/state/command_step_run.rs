// SPDX-License-Identifier: CC0-1.0

//! WHEN a step's command is answered for: the queue side of a `command-runner.jar` step.
//!
//! The status such a step ends in, and the session that answers it — a parse, a fixed
//! allow-list of session-local utilities and the AWS CLI's Amazon S3 transfers, with NO host
//! command, process or shell started anywhere — are [`super::step_command_status`]'s and
//! [`super::step_command_transfers`]'s. This module is only about when that answer is given
//! and what one call may spend giving it: a step's command is owed an answer from the instant
//! its cluster STARTED the step, and the answer is given by the next operation that reports
//! that step or its cluster, with no store guard held — exactly as a submitted Spark
//! application's run is ([`super::spark_step_run`]).
//!
//! A step whose command has no answer yet does not END ([`StepRecord::awaits_command`]): the
//! state it ends in is the status its command answered, so it stays `RUNNING` until there is
//! an answer rather than being reported COMPLETED or FAILED for a command nothing has
//! answered for.
//!
//! # Where a command is performed
//!
//! On a THREAD of its own, like a submitted application's run, because the transfers the
//! command's own program makes are calls into this emulator: the thread is not one of the
//! async runtime's workers, so those calls are driven by blocking it rather than by making
//! the session asynchronous. What ONE command's transfers may spend is that module's
//! ([`super::step_command_transfers::TRANSFER_BUDGET`]), and the wait for the thread is that
//! budget plus the grace below.
//!
//! How many such threads this process carries at once is bounded
//! ([`MAX_CONCURRENT_COMMAND_RUNS`], with a bounded wait for a permit), and one owed command is
//! performed once at a time however many callers report its step ([`COMMANDS_IN_FLIGHT`]):
//! a command's transfers are real effects on the object store, so neither a second caller nor a
//! call that has given up waiting makes them twice.

use super::*;

/// How many commands ONE call answers for, leaving the rest to the next call that reports the
/// cluster. CHOSEN: the ceiling on what one REQUEST costs, beside the session's own ceiling
/// on what one command costs. It is above the steps a cluster runs at once by a wide margin,
/// so a realistic queue settles in the call that reports it.
pub const MAX_COMMANDS_RESOLVED_PER_CALL: usize = 32;

/// How long ONE call spends performing owed commands before it leaves the rest to the next
/// call.
///
/// CHOSEN: the ceiling on what a REQUEST costs, beside the ceiling on what one command's
/// transfers cost. A further command is performed only while a whole one still fits inside
/// this span, so the longest a reporting operation can be held is this span plus the grace one
/// command is given rather than the product of the per-command budget and the count above.
pub const COMMAND_CALL_CEILING: Duration = Duration::from_secs(8);

/// How long after its budget is spent a command's thread is still waited for. CHOSEN: the
/// room a call that has just been given up on needs to come back, so a command that answered
/// at its bound is still read rather than being reported as a command nothing answered for.
pub const COMMAND_ABANDON_GRACE: Duration = Duration::from_secs(2);

/// How many commands this process performs AT ONCE.
///
/// CHOSEN, the sibling of the Spark path's own ceiling
/// ([`super::spark_step_run::MAX_CONCURRENT_APPLICATION_RUNS`]): a command is a session, a
/// thread and the calls the two make, so the number of them this process carries follows from
/// this bound rather than from how many callers report a cluster at once. The permit is held by
/// the command's own thread and given back when that thread ends.
pub const MAX_CONCURRENT_COMMAND_RUNS: usize = 4;

/// How long a command waits for one of [`MAX_CONCURRENT_COMMAND_RUNS`] to come free before
/// nothing is performed for it. CHOSEN: a bounded wait, so the operation that performs it
/// answers either way; a command that found no free slot stays owed an answer and the next call
/// performs it.
pub const COMMAND_PERMIT_WAIT: Duration = Duration::from_secs(2);

/// The permits [`MAX_CONCURRENT_COMMAND_RUNS`] hands out, for the whole process.
///
/// Held as an owned permit MOVED INTO the thread that performs the command, so it is given back
/// when that thread really ends — including a command the caller has already given up waiting
/// for, which would otherwise take this bound past its count while its thread was still going.
static COMMAND_RUN_PERMITS: ::std::sync::LazyLock<::std::sync::Arc<::tokio::sync::Semaphore>> =
    ::std::sync::LazyLock::new(|| {
        ::std::sync::Arc::new(::tokio::sync::Semaphore::new(MAX_CONCURRENT_COMMAND_RUNS))
    });

/// One command this service owes an answer: which step submitted it, what it is, and the
/// runtime role it carries of its own.
#[derive(Debug, Clone)]
struct StepCommandPlan {
    /// The step that submitted it.
    step_id: String,
    /// The command line the step's arguments are.
    command: StepCommand,
    /// The step's own runtime role, where it carries one ("the Amazon Resource Name (ARN) of
    /// the runtime role for a step on the cluster", `Step$ExecutionRoleArn`), which is what
    /// its calls are then signed as. A step carrying none presents what the cluster's
    /// instances obtain from their instance profile instead ([`cluster_step_credentials`]).
    execution_role_arn: Option<String>,
}

impl ClusterRecord {
    /// Every command of this cluster that has no answer yet, in queue order: which step
    /// submitted it, what it is, and the runtime role it carries of its own.
    ///
    /// A step is owed an answer once the cluster has STARTED it and nothing has answered for
    /// it yet ([`StepRecord::awaits_command`]); a step that has not started, has already been
    /// answered for, or is stopping is owed none.
    fn unanswered_step_commands(&self) -> Vec<StepCommandPlan> {
        self.steps
            .iter()
            .filter(|step| step.awaits_command() && step.start_date_time.is_some())
            .filter_map(|step| {
                Some(StepCommandPlan {
                    step_id: step.id.clone(),
                    command: step.command.clone()?,
                    execution_role_arn: step.execution_role_arn.clone(),
                })
            })
            .collect()
    }
}

/// Answer for every command of the cluster `cluster_id` whose step has started, and record
/// what each answered.
///
/// WHAT A COMMAND REACHES: a parse, a fixed allow-list of session-local utilities
/// ([`super::step_command_status`], whose header states it in full) and the AWS CLI's Amazon
/// S3 transfers ([`super::step_command_transfers`]), which are really made against this
/// emulator's own object store over the in-process callback. No host command, process or
/// shell is started, here or anywhere this calls. A command outside that list is refused with
/// the status a shell gives a command it could not find, and its step then reports that its
/// work was not run.
///
/// Called by the operations that report a step or its cluster, BEFORE they take this
/// service's store, and never with it held: the bounds that confine a command stand between
/// one caller's program and every other caller's latency rather than inside this service's
/// lock, and neither the session nor the object-store call it makes may happen under this
/// service's lock. The store is taken only to read the unanswered commands and to write the
/// outcomes, and this answers nothing at all for a cluster id this account holds no cluster
/// under.
///
/// Every transition this leads to was owed whether or not anybody called — the command was
/// owed from the instant the cluster STARTED the step — so a reporting operation may apply
/// it: what it adds is the step's own answer, which is what the step then ends in
/// ([`StepRecord::run_between`]). What ONE call answers for is bounded
/// ([`MAX_COMMANDS_RESOLVED_PER_CALL`], [`COMMAND_CALL_CEILING`]); a step whose turn did not
/// come stays owed an answer and the next call gives it.
pub async fn resolve_due_step_commands<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cluster_id: &str,
    now: SystemTime,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let (unanswered, job_flow_role): (Vec<StepCommandPlan>, String) = {
        let mut store = state.lock();
        // The queue is run up to this instant first, so a step whose turn has come is
        // STARTED and its command is among the unanswered ones.
        run_due_steps(engine, &mut store, cluster_id, now);
        match store.clusters.get(cluster_id) {
            Some(cluster) => (
                cluster.unanswered_step_commands(),
                cluster.job_flow_role.clone().unwrap_or_default(),
            ),
            None => (Vec::new(), String::new()),
        }
    };
    if unanswered.is_empty() {
        return;
    }
    // What the cluster's Amazon EC2 instances obtain from their instance profile, resolved
    // once for this call: the credential a step carrying no runtime role of its own presents,
    // which is nothing at all where that profile carries no role those instances may assume.
    let instance_credentials = cluster_step_credentials(ctx, region, &job_flow_role).await;
    // What this CALL may spend performing them: a further command is started only while a
    // whole one still fits, so the rest stay owed to the next call.
    let started = ::std::time::Instant::now();
    for plan in unanswered.into_iter().take(MAX_COMMANDS_RESOLVED_PER_CALL) {
        if started.elapsed().saturating_add(TRANSFER_BUDGET) > COMMAND_CALL_CEILING {
            return;
        }
        // A step's own runtime role is what its calls are signed as where it carries one;
        // every other step presents what the cluster's instances obtained.
        let credentials = match plan.execution_role_arn.as_deref() {
            Some(role_arn) => StepCredentials::Role(role_arn.to_string()),
            None => instance_credentials.clone(),
        };
        // The CLI this command's environment provides, built per command so each gets its own
        // transfer budget, and presenting that credential on every call it makes.
        let cli = step_object_cli(engine, ctx, region, &credentials, &plan.step_id);
        // One command is performed at a time, so a command another call is still performing —
        // including one that call has already given up waiting for — is not performed a second
        // time: its transfers are real effects on the object store, and a `mv` or an `rm`
        // applied twice is not what the command line asked for.
        let Some(claim) = claimed(cluster_id, &plan.step_id) else {
            continue;
        };
        let Some(outcome) = performed_command(&plan, cli, claim).await else {
            // Nothing answered: the step stays owed an answer, so a later call performs the
            // command rather than this one reporting an outcome nothing produced.
            continue;
        };
        let mut store = state.lock();
        if let Some(step) = store
            .clusters
            .get_mut(cluster_id)
            .and_then(|cluster| cluster.step_mut(&plan.step_id))
        {
            // Only a step that STILL awaits an answer takes this one: in the window the guard
            // was released another call may have answered the same command, or the caller may
            // have cancelled the step or its cluster, and neither is overwritten by an answer
            // computed before that happened.
            if step.awaits_command() {
                step.outcome = Some(outcome);
            }
        }
        // The steps can now reach the state their commands answered, if their spans have
        // already passed.
        run_due_steps(engine, &mut store, cluster_id, now);
    }
}

/// The commands being performed RIGHT NOW, as `<cluster>/<step>`, for the whole process.
///
/// A command's transfers are real effects on the object store, so one owed command is
/// performed once at a time however many callers report its step: a second call leaves it
/// owed and the answer the first produces is the one the step takes.
static COMMANDS_IN_FLIGHT: ::std::sync::LazyLock<
    ::std::sync::Mutex<::std::collections::BTreeSet<String>>,
> = ::std::sync::LazyLock::new(|| ::std::sync::Mutex::new(::std::collections::BTreeSet::new()));

/// A claim on performing ONE step's command, given up when the THREAD that performs it really
/// ends — including a command the caller has already stopped waiting for, which is still
/// making its calls until then.
#[derive(Debug)]
struct InFlight {
    key: String,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        held_commands().remove(&self.key);
    }
}

/// The in-flight set, recovering a guard a panicking command left poisoned: one caller's
/// command must not stop every later one from being performed.
fn held_commands() -> ::std::sync::MutexGuard<'static, ::std::collections::BTreeSet<String>> {
    COMMANDS_IN_FLIGHT
        .lock()
        .unwrap_or_else(::std::sync::PoisonError::into_inner)
}

/// Claim the command of `step_id` for this call, or `None` when it is already being performed.
fn claimed(cluster_id: &str, step_id: &str) -> Option<InFlight> {
    let key = format!("{cluster_id}/{step_id}");
    held_commands()
        .insert(key.clone())
        .then_some(InFlight { key })
}

/// Perform ONE owed command and answer what its step ends in, or `None` when nothing answered
/// for it at all.
///
/// The session is synchronous and makes calls of its own, so it runs on a thread with no
/// runtime context of its own and the caller's wait for it is bounded by the budget one
/// command's transfers have plus [`COMMAND_ABANDON_GRACE`]. `claim` is this command's place in
/// [`COMMANDS_IN_FLIGHT`] and the permit is its share of [`MAX_CONCURRENT_COMMAND_RUNS`]; both
/// are MOVED into that thread, so both are given up when the thread really ends rather than
/// when this call stops waiting. A command that found no free slot inside
/// [`COMMAND_PERMIT_WAIT`] is performed by nothing here and stays owed an answer.
async fn performed_command(
    plan: &StepCommandPlan,
    cli: Option<StepObjectCli>,
    claim: InFlight,
) -> Option<StepOutcome> {
    let permit = ::tokio::time::timeout(
        COMMAND_PERMIT_WAIT,
        ::std::sync::Arc::clone(&COMMAND_RUN_PERMITS).acquire_owned(),
    )
    .await
    .ok()?
    .ok()?;
    let command = plan.command.clone();
    let (sender, receiver) = ::tokio::sync::oneshot::channel();
    let spawned = ::std::thread::Builder::new()
        .name("emr-step-command".to_string())
        .spawn(move || {
            // Both held for as long as the command is being performed, whoever is still
            // waiting for it.
            let _claim = claim;
            let _permit = permit;
            // A command the session answered for nothing of ends the step the way every
            // unaccounted-for submission of this service ends it: in the modelled failure
            // state saying its work was not run, never a status nothing produced.
            let outcome = command_outcome(&command, cli).unwrap_or_else(not_executed);
            let _ = sender.send(outcome);
        });
    if spawned.is_err() {
        return None;
    }
    match ::tokio::time::timeout(
        TRANSFER_BUDGET.saturating_add(COMMAND_ABANDON_GRACE),
        receiver,
    )
    .await
    {
        Ok(Ok(outcome)) => Some(outcome),
        // The command was given up on, or its thread ended without reporting: nothing is
        // known about what it came to, so nothing is claimed about it.
        Ok(Err(_)) | Err(_) => None,
    }
}
