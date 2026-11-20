// SPDX-License-Identifier: CC0-1.0

//! The step actions a HOST run performs: the one Amazon Web Services API call an
//! `aws:executeAwsApi` step makes, and the script an `aws:executeScript` step runs.
//!
//! The walk in [`super`] is pure computation under this store's lock, so the two actions
//! whose work is neither — one dispatches a request into this emulator, the other runs
//! caller-supplied code — cannot happen there. The walk resolves such a step's inputs and
//! PARKS at it ([`super::HostAction`]); this module is what then runs it, with no guard
//! held, and records what it came to ([`super::AutomationExecutionRecord::record_host_action`]).
//!
//! # When a parked step runs
//!
//! The instant the walk reached it. Running the step was owed to the execution from then on,
//! whoever calls next, so every operation that reports or signals an automation performs
//! what is owed before it answers ([`advance_automation_actions`]) — exactly as the walk
//! itself is materialised before a read reports it, and beside the sibling drive the one
//! action whose work is another service's needs ([`super::instance_state::drive_due`], which
//! the same operations call). What ONE call performs is bounded three ways, so a request's
//! cost does not grow with what the Region holds: it performs the owed runs of the execution
//! the request is ABOUT (and of the children a rate-controlled one runs, whose outcome is
//! that parent's own status), at most [`MAX_ACTIONS_PER_CALL`] of them, and only while it has
//! [`ACTIONS_CALL_CEILING`] left; the rest stay owed for the next call. A step one call TOOK
//! is not taken by another until that caller has recorded it or its claim has lapsed
//! ([`super::ACTION_CLAIM_LEASE`]), so one step's action runs once.
//!
//! A step's own call is dispatched back into this same emulator, so it may reach an operation
//! that performs owed runs itself. That nesting is bounded by the framework's own callback
//! recursion limit, and by the claim above: a step another caller is running is not taken
//! again, so no run performs its own step.
//!
//! # What runs it
//!
//! Neither engine is this crate's:
//!
//! * the API call is dispatched through the ONE generic in-process AWS API seam this
//!   workspace has (the Lambda service crate's `state::execution::aws_api`), which builds
//!   the request the called service's own protocol describes, signs it as the role the
//!   automation runs as, and hands it to this same emulator over the framework's in-process
//!   callback. No socket is opened and nothing outside this process is reachable from it;
//! * the script is run by the ONE contained Python interpreter this workspace embeds (that
//!   same crate's `state::execution::python`), on a thread of its own, with the AWS SDK
//!   modules that runtime supplies on the import path.
//!
//! Every bound and every containment of a script's run is that engine's, in host code no
//! script can reach: the native modules it never registers, the in-memory operating-system
//! modules it supplies instead, and the deadline, step, memory, recursion and output
//! ceilings a run is given ([`script_budget`]). What this module adds is how much of ONE
//! AUTOMATION's work a run may be: how long one action may take ([`ACTION_CEILING`]), how
//! many scripts run at once ([`MAX_CONCURRENT_SCRIPTS`]), and how large a script and a
//! payload one step may carry.
//!
//! Nothing here is synthesised. A step's `Outputs` are selected out of what the API or the
//! script really answered ([`selectors`]), and an action this module cannot run reports the
//! step as failed rather than as having produced anything.

use ::std::collections::BTreeMap;
use ::std::time::{Duration, Instant, SystemTime};

use ::serde_json::{Map, Value};

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    function_sdk_access, AwsAccess, INIT_PHASE_LIMIT,
};

/// The API call an `aws:executeAwsApi` step makes.
pub mod aws_api_step;

/// The script an `aws:executeScript` step runs.
pub mod script_step;

/// The values a step's declared `outputs` select out of what its action answered.
pub mod selectors;

/// How long ONE in-process API call an `aws:executeAwsApi` step makes may take, from the
/// instant this emulator begins it.
///
/// CHOSEN, not an AWS duration: a step's own `timeoutSeconds` is the runbook's, and a
/// request of this emulator cannot be held open for a span a document chose. This is the
/// span one such call is given; a SCRIPT run is bounded by its own spans instead
/// ([`SCRIPT_PERMIT_WAIT`] waiting for an interpreter, then [`SCRIPT_HOST_WAIT`] for the
/// thread it runs on), which are stated there.
pub const ACTION_CEILING: Duration = Duration::from_secs(10);

/// How many parked steps ONE call performs.
///
/// CHOSEN: a call that finds several executions parked performs this many and leaves the
/// rest to the next call, so no single request can be made to run an unbounded number of
/// steps.
pub const MAX_ACTIONS_PER_CALL: usize = 8;

/// How long ONE call spends performing parked steps before it leaves the rest to the next
/// call.
///
/// CHOSEN: the ceiling on what a REQUEST costs, beside the ceiling on what one step's own
/// run costs ([`ACTION_CEILING`] for a call, [`SCRIPT_HOST_WAIT`] for a script). A further
/// step is STARTED only while the call has this span left, so the worst case a request waits
/// is this span plus ONE action's own — a call that has already spent it answers rather than
/// beginning another run.
pub const ACTIONS_CALL_CEILING: Duration = Duration::from_secs(30);

/// How many automation scripts run at once in this process.
///
/// CHOSEN: a script runs per parked step, so without this the bounds below would be
/// multiplied by the number of automations an account starts. Two scripts run at a time and
/// the rest wait for a permit.
pub const MAX_CONCURRENT_SCRIPTS: usize = 2;

/// How long a parked step waits for one of [`MAX_CONCURRENT_SCRIPTS`], at most.
///
/// CHOSEN: the wait is bounded, so the call answers either way. A step that waited the whole
/// wait has not been run at all: it stays parked, so a later call runs it rather than this
/// one recording an outcome nothing produced.
pub const SCRIPT_PERMIT_WAIT: Duration = Duration::from_secs(5);

/// The permits [`MAX_CONCURRENT_SCRIPTS`] hands out, for the whole process.
///
/// Held as an owned permit MOVED INTO the thread that runs the script, so it is given back
/// when that thread really ends — including a run the caller has already given up waiting
/// for, which would otherwise take this bound past its count while its thread was still
/// going.
static SCRIPT_PERMITS: ::std::sync::LazyLock<::std::sync::Arc<::tokio::sync::Semaphore>> =
    ::std::sync::LazyLock::new(|| {
        ::std::sync::Arc::new(::tokio::sync::Semaphore::new(MAX_CONCURRENT_SCRIPTS))
    });

/// The longest ONE script may run in the RUN'S OWN time.
///
/// CHOSEN, not an AWS duration: the execution happens inside the call that performs it, so
/// this is the span this emulator gives a script. It is a span of the RUN's own time rather
/// than of this host's: it is read against a clock the run itself drives
/// ([`SCRIPT_CLOCK_TICK`]) and spent by the waits the script asks for, so what a step comes
/// to rest in is the same on an idle host and on a loaded one. What the run may take of THIS
/// host is bounded beside it ([`SCRIPT_HOST_CEILING`]).
pub const SCRIPT_RUN_CEILING: Duration = Duration::from_secs(5);

/// How far ONE reading of a script run's clock moves it.
///
/// The span a script's run may last ([`SCRIPT_RUN_CEILING`]) is spent by what the RUN does,
/// and a traced event is one of those. The tick is sized so that the whole step budget
/// ([`MAX_SCRIPT_STEPS`]) spends well under that span — which is what leaves a script that
/// computes without end to be answered by the step budget, a count of its OWN work, and
/// leaves the span itself for the waits a script asks for.
pub const SCRIPT_CLOCK_TICK: Duration = Duration::from_nanos(100);

/// How much of THIS HOST's own time ONE script run may take, past which the engine stops it
/// whatever its own clock reads.
///
/// CHOSEN: the span above is the RUN's own time, so it bounds no CPU of this host — a script
/// spending traced events, or sitting in one long native call that delivers none, would hold
/// a thread of this process for as long as the host took over it. This is the bound that
/// stops it, read from this host's own clock at every traced event and from the host's own
/// watch over the run. It is generous beside the span the engine's phases take in the run's
/// own time (`INIT_PHASE_LIMIT` standing the environment up, then [`SCRIPT_RUN_CEILING`] of
/// script), because a script that FINISHED must be reported on what it came to rather than
/// on this host's load.
pub const SCRIPT_HOST_CEILING: Duration = INIT_PHASE_LIMIT
    .saturating_add(SCRIPT_RUN_CEILING)
    .saturating_mul(HOST_CEILING_OVER_THE_ENGINE_SPAN);

/// How many times the engine's own spans the host ceiling allows for
/// ([`SCRIPT_HOST_CEILING`]).
const HOST_CEILING_OVER_THE_ENGINE_SPAN: u32 = 2;

/// How long the call that performs one script waits for the thread it runs on.
///
/// The host ceiling above plus the grace the engine's own stop needs to land: the stop is
/// latched by the host and RAISED at the next traced event the interpreter delivers, so a
/// thread the engine has stopped answers a moment after the ceiling rather than at it. A
/// thread that has still not answered by then is one this emulator gave up on, and the step
/// says so.
pub const SCRIPT_HOST_WAIT: Duration = SCRIPT_HOST_CEILING.saturating_add(SCRIPT_STOP_GRACE);

/// How long the engine's own stop is given to land, once the host ceiling has latched it.
pub const SCRIPT_STOP_GRACE: Duration = Duration::from_secs(5);

/// How many traced events one script may execute.
///
/// CHOSEN: large enough for any script a runbook computes a value with — the frozen standard
/// library's own import chain spends a large share of it before the script's first line —
/// and far below what the span the run's clock gives it allows for, so a script that loops
/// without end is answered by THIS budget rather than by a clock. It is a count of the
/// script's OWN work, so it is the same count on an idle host and on a loaded one.
pub const MAX_SCRIPT_STEPS: u64 = 2_000_000;

/// How much memory one script may add to this process. CHOSEN, for the same reason.
const MAX_SCRIPT_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// How much one script may write to the `/tmp` of the environment it runs in. CHOSEN, for the
/// same reason: that directory is a disk on a real instance and is held in this process's
/// memory here, so what one run leaves there is bounded like everything else it holds.
const MAX_SCRIPT_SCRATCH_BYTES: u64 = 256 * 1024 * 1024;

/// How deep one script may recurse. The interpreter's own default is below what the frozen
/// standard library's import chain needs, so this is the limit a run is given; the call that
/// would raise it is taken away by the engine before any script code runs.
const MAX_SCRIPT_RECURSION: usize = 2_000;

/// How much of a script's output is kept.
///
/// It reaches no response — a step reports its declared outputs, not what its script
/// printed — so this only bounds what one run holds in memory.
const MAX_SCRIPT_LOG_BYTES: usize = 64 * 1024;

/// How large a value one script may answer with, which is what its outputs are selected out
/// of.
const MAX_SCRIPT_ANSWER_BYTES: usize = 256 * 1024;

/// The stack a script's thread is given: well above the interpreter's clamped recursion
/// limit, so deep recursion in a script ends that run with the interpreter's own
/// `RecursionError` rather than touching the thread's guard page.
const SCRIPT_STACK_BYTES: usize = 16 * 1024 * 1024;

/// What one script run may spend.
#[must_use]
pub fn script_budget() -> ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::Budget
{
    ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::Budget {
        wall_clock: SCRIPT_RUN_CEILING,
        steps: MAX_SCRIPT_STEPS,
        memory_bytes: MAX_SCRIPT_MEMORY_BYTES,
        ephemeral_bytes: MAX_SCRIPT_SCRATCH_BYTES,
        log_bytes: MAX_SCRIPT_LOG_BYTES,
        response_bytes: MAX_SCRIPT_ANSWER_BYTES,
        recursion_limit: MAX_SCRIPT_RECURSION,
    }
}

/// ONE host run a caller has TAKEN: which step of which execution, and everything running
/// its action takes.
///
/// Copied out from under the store lock, because the run happens with no guard held.
#[derive(Debug, Clone)]
pub struct ClaimedAction {
    /// The execution the step belongs to.
    pub execution_id: String,
    /// The Region that execution runs in, which is the Region its call is made in.
    pub region: String,
    /// Which step of the execution, by index into its own step records.
    pub step_index: usize,
    /// The step's name, as the runbook declares it.
    pub step_name: String,
    /// The step's action ([`super::HOST_RUN_ACTIONS`]).
    pub action: String,
    /// The step's `inputs`, as the walk resolved them.
    pub inputs: Map<String, Value>,
    /// The outputs the step declares it produces.
    pub outputs: Vec<super::runbook::StepOutput>,
    /// The role the automation runs as ([`super::ASSUME_ROLE_PARAMETER`]), which is the
    /// identity its call is signed as and its script's own calls are made as; `None` where
    /// the runbook declares none.
    pub assume_role: Option<String>,
}

/// What one host run came to.
#[derive(Debug, Clone)]
pub enum ActionOutcome {
    /// The action ran and produced the step's declared outputs.
    Succeeded(BTreeMap<String, Vec<String>>),
    /// The action did not run, or ran and failed: this is why, as the step's
    /// `FailureMessage`.
    Failed(String),
    /// Nothing about the action was established: this emulator had no interpreter free for
    /// it. The step stays PARKED and a later call runs it, so no outcome is recorded for
    /// work nothing performed; the string is what that was, for the log.
    NotRun(String),
}

/// Perform what `region`'s automation executions are owed at this instant: every step the
/// walk has parked at, in turn, bounded by [`MAX_ACTIONS_PER_CALL`] and
/// [`ACTIONS_CALL_CEILING`].
///
/// `about` is the execution the REQUEST is about, which narrows what this call performs to
/// that execution and the children a rate-controlled one runs — so a read of one automation
/// is not held up performing the runs another automation of the same Region is owed. `None`
/// is every execution of the Region, which is what a report OF the Region is about.
///
/// Called by every operation that reports or signals an automation, BEFORE it reads the
/// store, with no guard held — the lock is taken to claim a step and again to record what
/// its run came to, and never across the run itself.
pub async fn advance_automation_actions<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    about: Option<&str>,
) {
    let Some(state) = crate::state::service_state(engine) else {
        return;
    };
    let began = Instant::now();
    for _ in 0..MAX_ACTIONS_PER_CALL {
        if began.elapsed() >= ACTIONS_CALL_CEILING {
            return;
        }
        let claimed = {
            let now = SystemTime::now();
            let mut store = state.lock();
            // Everything the clock already owed at `now` is materialised first, because
            // what the walk has reached is what decides whether a step is parked at all.
            store.materialize_due(now);
            store.claim_automation_action(region, about, now)
        };
        let Some(claimed) = claimed else {
            return;
        };
        let outcome = run_action(engine, ctx, region, &claimed).await;
        // A step nothing could be run for stays parked, so this call stops rather than
        // taking it again: the next call to reach the execution runs it.
        let parked = matches!(outcome, ActionOutcome::NotRun(_));
        // What this call did to the execution, as the one record of a step's action having
        // been performed: the ids and the action, all of them this store's own or the
        // runbook's, and nothing of the answer or the script.
        ::tracing::debug!(
            region = %claimed.region,
            execution = %claimed.execution_id,
            step = %claimed.step_name,
            action = %claimed.action,
            outcome = match &outcome {
                ActionOutcome::Succeeded(_) => "succeeded",
                ActionOutcome::Failed(_) => "failed",
                ActionOutcome::NotRun(_) => "stays owed its run",
            },
            "an automation step's action was performed"
        );
        let now = SystemTime::now();
        state
            .lock()
            .record_automation_action(&claimed, outcome, now);
        if parked {
            return;
        }
    }
}

/// Run ONE claimed step and answer what it came to.
async fn run_action<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    claimed: &ClaimedAction,
) -> ActionOutcome {
    let access = sdk_access(engine, ctx, region, claimed);
    match claimed.action.as_str() {
        super::ACTION_EXECUTE_AWS_API => aws_api_step::run(claimed, access.as_ref()).await,
        super::ACTION_EXECUTE_SCRIPT => script_step::run(claimed, access).await,
        // Nothing else is parked, so nothing else can reach this; a step that did would be
        // reported as the step whose action nothing ran, as the walk reports every other
        // action it does not run.
        action => ActionOutcome::Failed(format!(
            "The {action} action of step {} runs where this emulator does not reach, so nothing \
             ran for this step",
            claimed.step_name
        )),
    }
}

/// The seam this step's work reaches the rest of this emulator through: the in-process
/// callback, signing every call as the session of the role the automation runs as.
///
/// An automation whose runbook declares no `AutomationAssumeRole` has no role of its own, so
/// its calls are signed with the credential the framework hands this emulator's handlers, as
/// every other in-process call between two emulated services is. `None` where there is no
/// seam at all, which is a step that reports the call it could not make.
fn sdk_access<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    claimed: &ClaimedAction,
) -> Option<AwsAccess> {
    match claimed.assume_role.as_deref() {
        Some(role) => function_sdk_access(engine, ctx, region, role, &claimed.execution_id),
        None => AwsAccess::new(ctx.callback().clone(), region)
            .map(|access| access.addressed_at(ctx.endpoint())),
    }
}

/// A non-empty text input of a step, by member name.
pub(crate) fn text_input(inputs: &Map<String, Value>, member: &str) -> Option<String> {
    inputs
        .get(member)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}
