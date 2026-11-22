// SPDX-License-Identifier: CC0-1.0

//! Rotating a secret: whether this service may invoke the rotation function at all, and
//! the four steps it drives that function through.
//!
//! `RotateSecret` "configures and starts the asynchronous process of rotating the secret"
//! (this service's Smithy model), and the process itself is the rotation FUNCTION's: "Secrets
//! Manager invokes the Lambda rotation function" once per step, and the function does the work
//! of each — `createSecret` stages a new value under `AWSPENDING`, `setSecret` changes the
//! credential in the service that owns it, `testSecret` proves the new value works, and
//! `finishSecret` moves `AWSCURRENT` onto the staged version. Each invocation carries the
//! event AWS documents for it: the secret, the step, and the `ClientRequestToken` naming the
//! version the rotation creates.
//!
//! Two things follow, and they are the whole of this module.
//!
//! * **The invocation needs permission.** "You also need `lambda:InvokeFunction` permissions
//!   on the rotation function" (`RotateSecret`, this service's Smithy model), and the call is
//!   made by this service as its own service principal — so what has to grant it is the
//!   FUNCTION's resource-based policy, read through Lambda's own `GetPolicy` and decided by
//!   the shared evaluation ([`::service_common::iam_authorization`]) for the principal
//!   [`SERVICE_PRINCIPAL`], under the condition keys such a grant is scoped by
//!   (`aws:SourceArn` naming the secret being rotated, `aws:SourceAccount` its account). A
//!   function whose policy does not admit this service cannot be invoked, so the rotation
//!   cannot be started and the call is refused rather than accepted and silently abandoned.
//! * **The steps run off the response path.** The call ANSWERS while the rotation is in
//!   flight — it reports the id of the version the rotation will create, which does not exist
//!   yet — so the four invocations are handed to a task of its own ([`drive`]), exactly as the
//!   work behind any other asynchronous submission in this tree is. A rotation is in progress
//!   while it runs, which is what makes a second `RotateSecret` refused, and the secret is
//!   stamped as rotated only once the last step has returned.
//!
//! * **A rotation that is not to start now is still TESTED.** "If you set RotateImmediately to
//!   false, Secrets Manager tests the rotation configuration by running the testSecret step of
//!   the Lambda rotation function. This test creates an AWSPENDING version of the secret and
//!   then removes it" (`RotateSecretRequest$RotateImmediately`, this service's Smithy model) —
//!   so such a call invokes the function once, off the response path too
//!   ([`test_configuration`]), and leaves no version behind. The tasks of this module, the
//!   rotations and the tests alike, are bounded ([`MAX_TASKS_IN_FLIGHT`]).
//!
//!   One half of that sentence is NOT reproduced: the candidate the test would create and
//!   remove is never staged, so the event carries the id of a version that does not exist, and
//!   a rotation function whose `testSecret` step reads that version raises instead of passing.
//!   What a client learns is still what the test is for — its function was reached and ran —
//!   and the secret is left exactly as this service's own pinned behaviour requires: a deferred
//!   call stages no version and does not refuse the next one
//!   (`tests/all/secret_rotation.rs::rotate_immediately_decides_whether_a_rotation_is_started`),
//!   which a candidate staked for the length of an invocation would make untrue for any caller
//!   acting meanwhile. Staging it would mean revisiting that behaviour first.
//!
//! Nothing here invents a rotation: every version a rotation produces is one the rotation
//! function itself wrote through this service's own operations. The function is invoked ONCE
//! per step, and a step it RAN and did not complete ends the rotation there: that step's answer
//! is the customer's own code's, so the steps before it are not run through a second time
//! behind a client watching the function — what is left is the state the real service leaves a
//! failed rotation in (the current value still readable, `AWSPENDING` still staked,
//! `LastRotatedDate` unset), and that END is RECORDED on the secret
//! ([`super::Secret::stop_rotation`]), which is what makes the rotation one a later call may
//! re-drive by naming its candidate rather than one every later call is refused for
//! (`crate::ops::rotate_secret`). The reattempt the refusal of a call asking for a FURTHER
//! rotation speaks of is the SCHEDULE's: "if rotation fails, Secrets Manager retries the entire
//! rotation process multiple times. If rotation is unsuccessful, this date may be in the past"
//! (`DescribeSecretResponse$NextRotationDate`, this service's Smithy model) — a date, not a
//! span inside the call's own window. An invocation that reached NO function at all is the one
//! thing attempted again ([`ROTATION_ATTEMPTS`]), because it says nothing about the function,
//! and it records no end for the same reason: such a rotation is one this service is still
//! attempting. That one attempt starts at the FIRST step, since "Secrets Manager retries the
//! entire rotation process": a function that could not be reached at a later step is therefore
//! invoked again for the steps it had answered — the once-per-step reading above is about a
//! function that ANSWERED, and an unreachable function has not.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ::service_common::iam_authorization::{self as iam, Decision};
use ::smithy_server_runtime::EmbeddedEmulator;

/// The service principal Secrets Manager invokes a rotation function as: "Ensure that the
/// function policy grants access to the principal `secretsmanager.amazonaws.com`" (Secrets
/// Manager User Guide, *Permissions for rotation*).
pub const SERVICE_PRINCIPAL: &str = "secretsmanager.amazonaws.com";

/// The IAM action an invocation of the rotation function is authorized under: "You also need
/// lambda:InvokeFunction permissions on the rotation function" (`RotateSecret`, this
/// service's Smithy model).
const INVOKE_FUNCTION_ACTION: &str = "lambda:InvokeFunction";

/// What this service answers when the rotation function's policy does not let it invoke the
/// function: AWS's own sentence for that condition (Secrets Manager User Guide,
/// *Troubleshoot rotation*), whose only variable part is the principal it names.
#[must_use]
pub fn invoke_not_permitted_message() -> String {
    format!(
        "Secrets Manager cannot invoke the specified Lambda function. Ensure that the function \
         policy grants access to the principal {SERVICE_PRINCIPAL}."
    )
}

/// The steps a rotation drives the rotation function through, in the order AWS invokes them:
/// "Secrets Manager calls the Lambda rotation function four times, each time with a different
/// value for the Step parameter" (Secrets Manager User Guide, *How rotation works*).
pub const ROTATION_STEPS: [&str; 4] = ["createSecret", "setSecret", "testSecret", "finishSecret"];

/// The step a rotation that is NOT to start now is tested with: "If you set RotateImmediately
/// to false, Secrets Manager tests the rotation configuration by running the testSecret step
/// of the Lambda rotation function" (`RotateSecretRequest$RotateImmediately`, this service's
/// Smithy model) — one of the four above, invoked on its own.
pub const TEST_SECRET_STEP: &str = "testSecret";

/// The member of the rotation event naming the secret being rotated.
const EVENT_SECRET_ID: &str = "SecretId";

/// The member naming the version the rotation creates, which is the `ClientRequestToken`
/// `RotateSecret` staked `AWSPENDING` on.
const EVENT_CLIENT_REQUEST_TOKEN: &str = "ClientRequestToken";

/// The member naming which step of the rotation this invocation is.
const EVENT_STEP: &str = "Step";

/// How long after the call that started it a rotation begins invoking the rotation function.
///
/// `RotateSecret` "configures and STARTS the asynchronous process of rotating the secret"
/// (this service's Smithy model), so the first step is the next thing that happens: the
/// rotation is driven straight through its four steps, and what a client waits for is the
/// FUNCTION's own work rather than a span this service adds ahead of it. That matters to a
/// client that watches a rotation by one of its side effects — a backup copy an early step
/// writes, a credential a later step changes — because every span added here is added
/// between that side effect and the rotation being finished.
///
/// The span is therefore the smallest one that still leaves the rotation IN FLIGHT for a
/// caller acting immediately: the call answers with `AWSPENDING` staked on a version that
/// does not exist yet, and this keeps that state readable — and a second `RotateSecret`
/// refused for a rotation still in progress — for a caller whose next request follows at
/// once. The band this tree gives a transition it owes a caller is not this sub-span's to
/// meet: it is the ROTATION that is the transition, and it stays in flight for the whole of
/// its four invocations, which is where that band is spent. This is only the lead-in, so it
/// is this emulator's own figure (AWS states none), orders of magnitude below one invocation
/// of a rotation function.
const ROTATION_START_DELAY: Duration = Duration::from_millis(250);

/// How long one step of a rotation may hold the task that drives it.
///
/// The invocation itself is bounded by Lambda — the function's own configured timeout, and
/// the concurrency the function has room for — so this is the bound on the WAIT rather than
/// on the run: a step whose answer does not come inside it is given up on, and the rotation
/// ends unfinished rather than keeping a task of its own for as long as the function likes.
/// It is this emulator's own figure, chosen well above any rotation function's run and far
/// below a span a developer would call stuck.
const ROTATION_STEP_BUDGET: Duration = Duration::from_secs(30);

/// How many times a rotation whose invocations reached NO function is driven, the first attempt
/// included.
///
/// A retry starts again at the first step, which is what makes a rotation whose function could
/// not be reached at all finish once it can be. It is only for that case: a step the function
/// RAN and raised is the function's own answer, and re-running the steps it already answered
/// would invoke it several times per step where the service invokes it once
/// ([`Invoked::Raised`]).
///
/// AWS publishes neither the number of attempts nor their spacing, so both are this
/// emulator's own: enough for an obstacle in front of the function that clears by itself — a
/// function still inside the provisioning window its creation opened, a concurrency limit
/// momentarily reached — and few enough that a rotation whose function cannot be reached at all
/// stops being driven rather than being retried for as long as this process runs.
const ROTATION_ATTEMPTS: u32 = 4;

/// How long after an attempt that did not finish the rotation the next one begins. See
/// [`ROTATION_ATTEMPTS`]: it is this emulator's own span, above the windows a service of this
/// emulator opens on a resource it is still provisioning.
const ROTATION_RETRY_DELAY: Duration = Duration::from_secs(1);

/// The resource type a Lambda function's ARN names:
/// `arn:aws:lambda:<region>:<account>:function:<name>`.
const FUNCTION_RESOURCE_TYPE: &str = "function";

/// Lambda's ARN namespace, the vendor field of that ARN.
const LAMBDA_ARN_NAMESPACE: &str = "lambda";

/// Whether `arn` names a Lambda function, which is what a rotation function has to be:
/// a rotation drives it by INVOKING it, so a value that is not the ARN of a function names
/// nothing this service can invoke.
///
/// The account and Region fields are not compared: a `RotationLambdaARN` names the function
/// as the caller spelled it, and whether that function can be invoked is the FUNCTION's own
/// answer ([`invoke_permission_refusal`]) rather than a reading of its ARN.
#[must_use]
pub fn names_a_function(arn: &str) -> bool {
    ::service_common::arn::parse_of(arn, LAMBDA_ARN_NAMESPACE).is_some_and(|parsed| {
        parsed.resource_type() == FUNCTION_RESOURCE_TYPE
            && parsed
                .resource
                .strip_prefix(FUNCTION_RESOURCE_TYPE)
                .is_some_and(|tail| tail.len() > 1)
    })
}

/// The partition the ARN form this service states a malformed `RotationLambdaARN` against is
/// spelled in, which is the one this emulator serves.
const ARN_PARTITION: &str = "aws";

/// What this service answers for a `RotationLambdaARN` that names no Lambda function: the
/// FORM such a value has to have, so a caller reads back what it sent its ARN wrong against.
///
/// `RotationLambdaARNType` states no pattern and a length of 0..2048 (this service's Smithy
/// model), so which values name a rotation function is the service's judgement, and a value
/// that is not a Lambda function's ARN is a malformed parameter rather than a secret with no
/// rotation function: the two conditions travel under the same `InvalidRequestException`, so
/// the sentence is the only thing that tells a caller to fix the ARN it passed instead of
/// attaching a rotation function. The only part taken from the request is the REGION it was
/// made in; the account and the function name are placeholders of the stated form rather than
/// values read back out of the request.
#[must_use]
pub fn not_a_function_message(region: &str) -> String {
    format!(
        "Invalid rotation Lambda ARN. Ensure the ARN matches format \
         arn:{ARN_PARTITION}:{LAMBDA_ARN_NAMESPACE}:{region}:<account>:\
         {FUNCTION_RESOURCE_TYPE}:<functionName>."
    )
}

/// Why this service cannot invoke the rotation function `lambda_arn` for the secret
/// `secret_arn`, or `None` when it can.
///
/// The decision is the FUNCTION's own resource-based policy, read through Lambda's `GetPolicy`
/// — the same document `AddPermission` writes and that operation answers with — and evaluated
/// for the service principal this invocation is made as. A policy that allows
/// `lambda:InvokeFunction` to that principal for this secret permits the rotation; one that
/// does not, and a function that holds no policy at all, refuses it.
///
/// `None`, so nothing is refused on evidence that is not there, when the request reached no
/// Lambda at all: an emulator assembled without it, where the TRANSPORT refused the call and
/// nothing was said about the function.
pub async fn invoke_permission_refusal(
    callback: &EmbeddedEmulator,
    region: &str,
    lambda_arn: &str,
    secret_arn: &str,
) -> Option<String> {
    let answered = lambda_client(callback, region)
        .get_policy()
        .function_name(lambda_arn)
        .send()
        .await;
    let policy = match answered {
        Ok(answer) => answer.policy().unwrap_or_default().to_string(),
        Err(error) => {
            if reached_no_lambda(&error) {
                ::tracing::debug!(
                    function = %lambda_arn,
                    "no Lambda answered for this rotation function, so its policy is not read"
                );
                return None;
            }
            // Lambda answered: the function holds no policy, or there is no such function.
            // Either way nothing grants this service the invocation.
            ::tracing::debug!(
                function = %lambda_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the rotation function's policy could not be read, so the rotation is refused"
            );
            return Some(invoke_not_permitted_message());
        }
    };
    (!permits_invocation(&policy, lambda_arn, secret_arn)).then(invoke_not_permitted_message)
}

/// Whether `policy` lets [`SERVICE_PRINCIPAL`] invoke the function `lambda_arn` for the secret
/// `secret_arn`.
///
/// The condition keys a grant of this kind is scoped by travel with the request: a statement
/// written by `AddPermission` with a `SourceArn` names the ONE secret whose rotation may use
/// the function, and one with a `SourceAccount` the account that secret belongs to, so a grant
/// made for another secret does not admit this rotation.
fn permits_invocation(policy: &str, lambda_arn: &str, secret_arn: &str) -> bool {
    let principals = [SERVICE_PRINCIPAL.to_string()];
    let account = ::service_common::arn::parse(secret_arn)
        .map(|arn| arn.account.to_string())
        .unwrap_or_else(|| ::engine::emulator_account_id().to_string());
    let context = [
        (SOURCE_ARN_KEY.to_string(), secret_arn.to_string()),
        (SOURCE_ACCOUNT_KEY.to_string(), account),
    ];
    let request = iam::Request::new(INVOKE_FUNCTION_ACTION, lambda_arn, &principals)
        .with_context(&context)
        .with_published_keys(&[SOURCE_ARN_KEY, SOURCE_ACCOUNT_KEY]);
    iam::resource_decision(policy, &request) == Decision::Allowed
}

/// The global condition key naming the resource the call is made on behalf of, which for a
/// rotation is the secret being rotated ("aws:SourceArn", IAM's global condition keys).
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The global condition key naming that resource's account ("aws:SourceAccount").
const SOURCE_ACCOUNT_KEY: &str = "aws:SourceAccount";

/// Drive the rotation the call that returns this started: invoke the rotation function once
/// per step of [`ROTATION_STEPS`], and stamp the secret as rotated once the last step has
/// returned.
///
/// Returns at once: the rotation runs in a task of its own, because the call that started it
/// must answer while the rotation is still in flight. The caller hands over its own clone of
/// the in-process callback, so the invocations are made exactly where the starting call was
/// made.
///
/// The number of tasks this module has in flight is bounded ([`MAX_TASKS_IN_FLIGHT`]). A
/// rotation that finds no slot free is not driven, which leaves the secret in the state the
/// real service leaves a rotation whose function it could not reach — the current value still
/// readable, `AWSPENDING` staked and `LastRotatedDate` unset — and a client takes that rotation
/// up again the documented way: `CancelRotateSecret`, then `RotateSecret`.
pub fn drive<Eng: Send + Sync + 'static>(
    engine: Arc<Eng>,
    callback: EmbeddedEmulator,
    rotation: Rotation,
) {
    let Some(slot) = TaskSlot::claim() else {
        ::tracing::warn!(
            secret = %rotation.secret_arn,
            version = %rotation.version_id,
            "{MAX_TASKS_IN_FLIGHT} rotation tasks are already in flight, so this rotation is \
             not driven"
        );
        return;
    };
    ::tokio::spawn(async move {
        let _slot = slot;
        run(&engine, &callback, &rotation).await;
    });
}

/// Test the rotation CONFIGURATION a call that asked not to rotate now has just applied:
/// invoke the configured rotation function once, with the `testSecret` step.
///
/// "If you set RotateImmediately to false, Secrets Manager tests the rotation configuration by
/// running the testSecret step of the Lambda rotation function. This test creates an
/// AWSPENDING version of the secret and then removes it"
/// (`RotateSecretRequest$RotateImmediately`, this service's Smithy model) — so the function is
/// reached once, which is how a client that schedules a rotation learns at configuration time
/// that its function can be invoked and runs, and NOTHING of the test is left on the secret:
/// the version it creates is removed again, and the only writer of a version here is the
/// rotation function itself ([`drive`]).
///
/// Returns at once, for the same reason [`drive`] does: the call that configured the rotation
/// answers while the test is still in flight. It takes one of the same bounded slots
/// ([`MAX_TASKS_IN_FLIGHT`]): a call that finds none free configures the rotation exactly as it
/// would have and leaves the test unperformed, which changes nothing about the secret.
pub fn test_configuration(callback: EmbeddedEmulator, rotation: Rotation) {
    let Some(slot) = TaskSlot::claim() else {
        ::tracing::warn!(
            secret = %rotation.secret_arn,
            "{MAX_TASKS_IN_FLIGHT} rotation tasks are already in flight, so this configuration \
             is not tested"
        );
        return;
    };
    ::tokio::spawn(async move {
        let _slot = slot;
        run_test(&callback, &rotation).await;
    });
}

/// How many tasks this module may have in flight at once, over every secret: the rotations
/// being driven and the configuration tests together.
///
/// Each is one task per request, so without a ceiling the number of them is whatever a caller
/// asks for. Nothing else bounds it: a deferred call stakes no candidate, so the in-progress
/// refusal does not serialise one secret's deferred calls, and a rotation taken up again after
/// a cancel is served while the cancelled rotation's own task may still be ending.
///
/// The figure is this emulator's own, the same one its sibling services bound their one-shot
/// tasks by: far above the few a developer's client has outstanding, and small enough that the
/// tasks cannot grow without end.
///
/// The slot primitive below is the same one
/// `services/service-com-amazonaws-elasticache-amazonelasticachev9/src/state/event_notifications.rs`
/// and `services/service-com-amazonaws-cloudformation-cloudformation/src/state/stack_set_events.rs`
/// hold: a counter raised only from below the ceiling and given back on drop. It is written
/// here rather than reused because neither is reachable from this crate — a service crate does
/// not depend on another service's internals — and it belongs in a crate every service can
/// reach (`services/common`) rather than in a third copy; moving all three is its own change,
/// since each of those two services' tasks are bounded by their own ceiling today.
const MAX_TASKS_IN_FLIGHT: usize = 64;

/// The slots [`MAX_TASKS_IN_FLIGHT`] bounds.
static TASKS_IN_FLIGHT: ::std::sync::atomic::AtomicUsize =
    ::std::sync::atomic::AtomicUsize::new(0);

/// ONE of the [`MAX_TASKS_IN_FLIGHT`] slots, held for as long as the task that took it runs.
///
/// The slot is given back when this is DROPPED, so a task that ends early — or panics — cannot
/// leak it.
struct TaskSlot;

impl TaskSlot {
    /// Take one slot, or `None` when they are all taken: the count is only ever raised from
    /// below the ceiling, so two callers cannot both take the last one.
    fn claim() -> Option<Self> {
        TASKS_IN_FLIGHT
            .try_update(
                ::std::sync::atomic::Ordering::SeqCst,
                ::std::sync::atomic::Ordering::SeqCst,
                |held| (held < MAX_TASKS_IN_FLIGHT).then_some(held + 1),
            )
            .ok()
            .map(|_| Self)
    }
}

impl Drop for TaskSlot {
    fn drop(&mut self) {
        TASKS_IN_FLIGHT.fetch_sub(1, ::std::sync::atomic::Ordering::SeqCst);
    }
}

/// Invoke the rotation function once for the `testSecret` step, trying again only while the
/// invocation did not REACH the function.
///
/// A function that ran and raised has been tested — that is the answer the test was for — so it
/// is invoked once, as AWS invokes it once. An invocation that reached no function at all says
/// nothing about it, and is retried on the same terms a rotation's steps are
/// ([`ROTATION_ATTEMPTS`]): an obstacle in front of the function that clears by itself, such as
/// a function still inside the provisioning window its creation opened.
async fn run_test(callback: &EmbeddedEmulator, rotation: &Rotation) {
    ::tokio::time::sleep(ROTATION_START_DELAY).await;
    for attempt in 1..=ROTATION_ATTEMPTS {
        if invoke_step(callback, rotation, TEST_SECRET_STEP).await != Invoked::Unreached {
            return;
        }
        if attempt < ROTATION_ATTEMPTS {
            ::tokio::time::sleep(ROTATION_RETRY_DELAY).await;
        }
    }
}

/// One rotation in flight: which secret, under which version id, through which function.
#[derive(Debug, Clone)]
pub struct Rotation {
    /// The Region the secret lives in, which is where its rotation function is invoked.
    pub region: String,
    /// The secret's own ARN, which is what the rotation event names it by.
    pub secret_arn: String,
    /// The id `RotateSecret` staked `AWSPENDING` on: the version the rotation function
    /// creates, which the event carries as its `ClientRequestToken`.
    pub version_id: String,
    /// The rotation function this rotation invokes.
    pub lambda_arn: String,
    /// Which of the secret's STARTED rotations this is
    /// ([`Secret::rotation_generation`]): a rotation cancelled and then resumed is staked on
    /// the same version id with rotation enabled again, so this is what tells the task driving
    /// it that the secret is still waiting for ITS rotation rather than for a later one.
    ///
    /// [`Secret::rotation_generation`]: super::Secret::rotation_generation
    pub generation: u64,
}

/// Invoke each step in turn, stopping at the first one the function did not complete, and
/// drive the whole rotation again only when the attempt reached no function at all.
///
/// A rotation whose invocation reached no function is retried from its FIRST step
/// ([`ROTATION_ATTEMPTS`]), which is what gets one past an obstacle in front of the function
/// that clears by itself. A rotation whose function RAN and raised is not: that step's answer
/// is the function's own, so the rotation ends where the real service leaves such a rotation —
/// `AWSPENDING` staked, `AWSCURRENT` where it was and `LastRotatedDate` unset — because the
/// only writes to the secret are the ones the rotation function itself made through this
/// service's operations.
async fn run<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
    callback: &EmbeddedEmulator,
    rotation: &Rotation,
) {
    ::tokio::time::sleep(ROTATION_START_DELAY).await;
    for attempt in 1..=ROTATION_ATTEMPTS {
        // A rotation the secret is no longer waiting for is not driven: "if a rotation is
        // currently in progress, cancels the rotation" (`CancelRotateSecret`, this service's
        // Smithy model), and a secret that is gone awaits nothing either.
        if !still_awaited(engine, rotation) {
            ::tracing::debug!(
                secret = %rotation.secret_arn,
                version = %rotation.version_id,
                "this rotation is no longer the one the secret awaits, so it is not driven \
                 further"
            );
            return;
        }
        match drive_steps(engine, callback, rotation).await {
            // Every step returned, so the rotation COMPLETED: "LastRotatedDate — the last
            // date and time that Secrets Manager rotated the secret"
            // (`DescribeSecretResponse`, this service's Smithy model), and the schedule's
            // next due instant is computed from it.
            Attempt::Completed => {
                stamp_rotated(engine, rotation);
                return;
            }
            // The function ANSWERED a step and did not complete it: that step's answer is the
            // customer's own code's, so the rotation ENDS there with the terminal outcome
            // recorded ([`stamp_stopped`]) — the labels stay where that function left them, and
            // what the record says is that no rotation of this secret is in flight any more.
            Attempt::Answered => {
                stamp_stopped(engine, rotation);
                return;
            }
            // The secret stopped awaiting this rotation (cancelled, resumed or gone), so
            // nothing further is this task's to do and nothing of it is this task's to record.
            Attempt::Abandoned => return,
            // The invocation reached no function at all, which says nothing about the
            // function: the whole rotation is driven again.
            Attempt::Unreached => {
                if attempt < ROTATION_ATTEMPTS {
                    ::tokio::time::sleep(ROTATION_RETRY_DELAY).await;
                }
            }
        }
    }
}

/// What ONE attempt at driving a rotation came to, which is what decides whether another is
/// made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// Every step of the protocol returned.
    Completed,
    /// A step's invocation reached the function and the function did not complete it.
    Answered,
    /// A step's invocation reached no running function at all.
    Unreached,
    /// The secret no longer awaits this rotation.
    Abandoned,
}

/// Invoke the four steps in order, and report what the attempt came to.
///
/// The secret is asked BEFORE every step whether it still awaits this rotation: "if a rotation
/// is currently in progress, cancels the rotation" (`CancelRotateSecret`, this service's Smithy
/// model), so a cancel takes effect at the next step rather than only at the next attempt — a
/// cancelled rotation must not go on to move `AWSCURRENT` onto the candidate it left behind.
async fn drive_steps<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
    callback: &EmbeddedEmulator,
    rotation: &Rotation,
) -> Attempt {
    for step in ROTATION_STEPS {
        if !still_awaited(engine, rotation) {
            ::tracing::debug!(
                secret = %rotation.secret_arn,
                version = %rotation.version_id,
                step,
                "this rotation is no longer the one the secret awaits, so this step is not run"
            );
            return Attempt::Abandoned;
        }
        match invoke_step(callback, rotation, step).await {
            Invoked::Completed => {}
            Invoked::Raised => return Attempt::Answered,
            Invoked::Unreached => return Attempt::Unreached,
        }
    }
    Attempt::Completed
}

/// Whether the secret is still waiting for THIS rotation: it exists, rotation is on, the
/// version this rotation creates is the one `AWSPENDING` names, and the rotation STARTED for it
/// is still this one rather than a later one ([`Rotation::generation`]).
///
/// Read under the store's guard, between invocations rather than across one, so a rotation
/// cancelled while a step was in flight is not driven again, and a rotation another call took
/// over leaves its predecessor's task nothing to do.
fn still_awaited<Eng: Send + Sync + 'static>(engine: &Arc<Eng>, rotation: &Rotation) -> bool {
    let Some(state) = super::service_state(&**engine) else {
        return false;
    };
    let store = state.lock();
    store
        .get(&rotation.region, &rotation.secret_arn)
        .is_some_and(|secret| {
            secret.rotation_on()
                && secret.rotation_generation == rotation.generation
                && secret.pending_version_id() == Some(rotation.version_id.as_str())
        })
}

/// Stamp the secret as rotated, at the instant its rotation finished.
fn stamp_rotated<Eng: Send + Sync + 'static>(engine: &Arc<Eng>, rotation: &Rotation) {
    let Some(state) = super::service_state(&**engine) else {
        return;
    };
    let now = SystemTime::now();
    let mut store = state.lock();
    if let Some(secret) = store.get_mut(&rotation.region, &rotation.secret_arn) {
        secret.finish_rotation(now);
    }
}

/// Record that this rotation ENDED without finishing, because its function answered a step it
/// did not complete ([`super::Secret::stop_rotation`]).
///
/// It is recorded against the rotation's own generation, so a secret that has since moved on to
/// another rotation keeps what that one is doing. Nothing else about the secret moves: the state
/// a failed rotation leaves is the one its function left.
fn stamp_stopped<Eng: Send + Sync + 'static>(engine: &Arc<Eng>, rotation: &Rotation) {
    let Some(state) = super::service_state(&**engine) else {
        return;
    };
    let mut store = state.lock();
    if let Some(secret) = store.get_mut(&rotation.region, &rotation.secret_arn) {
        secret.stop_rotation(rotation.generation);
    }
}

/// What one invocation of the rotation function came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Invoked {
    /// The function ran and returned, so the step it was invoked for happened.
    Completed,
    /// The function RAN and raised: "if present, indicates that an error occurred during
    /// function execution" (`FunctionError`, Lambda's `Invoke` model), which is a step of the
    /// rotation that did not happen.
    Raised,
    /// The invocation reached no running function at all — Lambda refused it, or no answer came
    /// inside the budget — so nothing was said about the function.
    Unreached,
}

/// Invoke the rotation function for one step, and report what that invocation came to.
async fn invoke_step(callback: &EmbeddedEmulator, rotation: &Rotation, step: &str) -> Invoked {
    let event = ::serde_json::json!({
        EVENT_STEP: step,
        EVENT_SECRET_ID: rotation.secret_arn,
        EVENT_CLIENT_REQUEST_TOKEN: rotation.version_id,
    });
    let answered = lambda_client(callback, &rotation.region)
        .invoke()
        .function_name(&rotation.lambda_arn)
        .invocation_type(::aws_sdk_lambda::types::InvocationType::RequestResponse)
        .payload(::aws_sdk_lambda::primitives::Blob::new(
            event.to_string().into_bytes(),
        ))
        .send();
    // The budget is this service's own, not the function's: a step still running when it
    // elapses is not waited for any longer, and the rotation it belongs to is left
    // unfinished.
    let Ok(answered) = ::tokio::time::timeout(ROTATION_STEP_BUDGET, answered).await else {
        ::tracing::warn!(
            secret = %rotation.secret_arn,
            step,
            "the rotation function did not answer this step inside its budget, so the \
             rotation did not finish"
        );
        return Invoked::Unreached;
    };
    match answered {
        Ok(output) => match output.function_error() {
            None => Invoked::Completed,
            Some(function_error) => {
                ::tracing::warn!(
                    secret = %rotation.secret_arn,
                    step,
                    function_error,
                    "the rotation function raised on this step, so the rotation did not finish"
                );
                Invoked::Raised
            }
        },
        Err(error) => {
            ::tracing::warn!(
                secret = %rotation.secret_arn,
                step,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the rotation function could not be invoked for this step"
            );
            Invoked::Unreached
        }
    }
}

/// The error codes that are the in-process TRANSPORT's own refusal rather than an answer from
/// Lambda: this emulator holds no Lambda at the endpoint that was called, or more than one of
/// its services claims it. Either way the request reached no Lambda, so it says nothing about
/// the function — the same reading every service that calls another in process makes of these
/// two codes.
const TRANSPORT_REFUSALS: [&str; 2] = ["UnknownService", "AmbiguousService"];

/// Whether an SDK failure is one of those transport refusals.
fn reached_no_lambda<E, R>(error: &::aws_sdk_lambda::error::SdkError<E, R>) -> bool
where
    E: ::aws_sdk_lambda::error::ProvideErrorMetadata,
{
    ::aws_sdk_lambda::error::ProvideErrorMetadata::code(error)
        .is_some_and(|code| TRANSPORT_REFUSALS.contains(&code))
}

/// Lambda's endpoint prefix, the first label of its regional endpoint.
const LAMBDA_ENDPOINT_PREFIX: &str = "lambda";

/// Lambda's client for `region`, over the in-process callback into this same emulator: a
/// rotation function is one this emulator holds, so the invocation never leaves the process.
///
/// The transport is in-process, so this disables what a network client needs and this one
/// cannot have: retries, timeouts, stalled-stream protection and identity caching each need a
/// sleep implementation the callback transport is not given, and there is nothing to retry
/// behind it anyway.
fn lambda_client(callback: &EmbeddedEmulator, region: &str) -> ::aws_sdk_lambda::Client {
    let config = ::aws_sdk_lambda::Config::builder()
        .behavior_version(::aws_sdk_lambda::config::BehaviorVersion::latest())
        .region(::aws_sdk_lambda::config::Region::new(region.to_string()))
        .endpoint_url(format!(
            "https://{LAMBDA_ENDPOINT_PREFIX}.{region}.amazonaws.com"
        ))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_lambda::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_lambda::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_lambda::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_lambda::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_lambda::Client::from_conf(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A function policy statement granting `lambda:InvokeFunction` to `principal`, with the
    /// `Condition` block given (none when it is empty).
    fn policy(principal: &str, resource: &str, condition: &str) -> String {
        let clause = if condition.is_empty() {
            String::new()
        } else {
            format!(r#","Condition":{condition}"#)
        };
        format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"rotate","Effect":"Allow",
             "Principal":{{"Service":"{principal}"}},"Action":"lambda:InvokeFunction",
             "Resource":"{resource}"{clause}}}]}}"#
        )
    }

    /// RULE: the invocation is permitted exactly when the FUNCTION's policy allows
    /// `lambda:InvokeFunction` to this service's principal for the secret being rotated.
    ///
    /// The rows differ along who the statement grants to, which function it names, and how it
    /// is scoped: unscoped, scoped to this secret, scoped to another secret, scoped to the
    /// account, and the shape `AddPermission` writes (`ArnLike` over `AWS:SourceArn`).
    #[test]
    fn a_rotation_is_permitted_only_by_the_functions_own_grant() {
        let function = "arn:aws:lambda:us-west-2:000000000000:function:rotator";
        let secret = "arn:aws:secretsmanager:us-west-2:000000000000:secret:db-AbCdEf";
        let other = "arn:aws:secretsmanager:us-west-2:000000000000:secret:other-GhIjKl";
        // (case, the document, whether the rotation may be invoked)
        let rows: Vec<(&str, String, bool)> = vec![
            ("no policy at all", String::new(), false),
            (
                "an unscoped grant to this service",
                policy(SERVICE_PRINCIPAL, function, ""),
                true,
            ),
            (
                "a grant to another service principal",
                policy("events.amazonaws.com", function, ""),
                false,
            ),
            (
                "a grant on another function",
                policy(
                    SERVICE_PRINCIPAL,
                    "arn:aws:lambda:us-west-2:000000000000:function:elsewhere",
                    "",
                ),
                false,
            ),
            (
                "scoped to this secret the way AddPermission writes it",
                policy(
                    SERVICE_PRINCIPAL,
                    function,
                    &format!(r#"{{"ArnLike":{{"AWS:SourceArn":"{secret}"}}}}"#),
                ),
                true,
            ),
            (
                "scoped to another secret",
                policy(
                    SERVICE_PRINCIPAL,
                    function,
                    &format!(r#"{{"ArnLike":{{"AWS:SourceArn":"{other}"}}}}"#),
                ),
                false,
            ),
            (
                "scoped to the secret's own account",
                policy(
                    SERVICE_PRINCIPAL,
                    function,
                    r#"{"StringEquals":{"AWS:SourceAccount":"000000000000"}}"#,
                ),
                true,
            ),
            (
                "scoped to another account",
                policy(
                    SERVICE_PRINCIPAL,
                    function,
                    r#"{"StringEquals":{"AWS:SourceAccount":"111122223333"}}"#,
                ),
                false,
            ),
            (
                "an explicit deny of the same action",
                policy(SERVICE_PRINCIPAL, function, "").replace("Allow", "Deny"),
                false,
            ),
        ];
        for (case, document, permitted) in rows {
            let reached = permits_invocation(&document, function, secret);
            assert_eq!(
                reached, permitted,
                "case {case:?}: expected permitted={permitted}, got {reached} for {document}"
            );
        }
    }

    /// RULE: the four steps are the ones AWS invokes a rotation function with, in that order.
    #[test]
    fn the_steps_are_the_documented_four_in_order() {
        assert_eq!(
            ROTATION_STEPS,
            ["createSecret", "setSecret", "testSecret", "finishSecret"],
            "case \"the rotation steps\": expected AWS's four steps in order, got {ROTATION_STEPS:?}"
        );
    }

    /// RULE: the step a deferred rotation tests the configuration with is one of those four.
    #[test]
    fn the_test_step_is_one_of_the_documented_steps() {
        assert!(
            ROTATION_STEPS.contains(&TEST_SECRET_STEP),
            "case \"the test step\": expected one of {ROTATION_STEPS:?}, got {TEST_SECRET_STEP:?}"
        );
    }

    /// RULE: a `RotationLambdaARN` that names no function is answered with the FORM such a
    /// value has to have, whose only variable part is the Region the call was made in.
    ///
    /// The rows run over Regions of different lengths, since the Region is what varies.
    #[test]
    fn a_malformed_rotation_arn_states_the_form_for_the_requests_region() {
        for region in ["us-west-2", "eu-central-1", "ap-southeast-4"] {
            let stated = not_a_function_message(region);
            let expected = format!(
                "Invalid rotation Lambda ARN. Ensure the ARN matches format \
                 arn:aws:lambda:{region}:<account>:function:<functionName>."
            );
            assert_eq!(
                stated, expected,
                "case {region:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }

    /// RULE: a value names a rotation function exactly when it is the ARN of a Lambda
    /// function, which is what an invocation can be made against.
    /// The rows run over the forms a caller sends: a function ARN, a function ARN with a
    /// qualifier, ARNs of other Lambda resources and of other services, and values that are no
    /// ARN at all.
    #[test]
    fn only_a_lambda_functions_arn_names_a_rotation_function() {
        // (the value, whether it names a function)
        let rows: &[(&str, bool)] = &[
            ("arn:aws:lambda:us-west-2:000000000000:function:rotator", true),
            (
                "arn:aws:lambda:eu-central-1:111122223333:function:rotator:PROD",
                true,
            ),
            ("arn:aws:lambda:us-west-2:000000000000:function:", false),
            (
                "arn:aws:lambda:us-west-2:000000000000:layer:shared:1",
                false,
            ),
            (
                "arn:aws:secretsmanager:us-west-2:000000000000:secret:db-AbCdEf",
                false,
            ),
            ("not-a-lambda-arn-at-all", false),
            ("", false),
            ("arn:aws:lambda", false),
        ];
        for (value, names) in rows {
            let read = names_a_function(value);
            assert_eq!(
                read, *names,
                "case {value:?}: expected names_a_function={names}, got {read}"
            );
        }
    }

    /// RULE: the tasks in flight are bounded, and a slot is given back when the task that took
    /// it ends.
    #[test]
    fn the_tasks_in_flight_are_bounded() {
        let held: Vec<TaskSlot> = (0..MAX_TASKS_IN_FLIGHT)
            .filter_map(|_| TaskSlot::claim())
            .collect();
        assert_eq!(
            held.len(),
            MAX_TASKS_IN_FLIGHT,
            "case \"up to the ceiling\": expected {MAX_TASKS_IN_FLIGHT} slots to be free, got {}",
            held.len()
        );
        assert!(
            TaskSlot::claim().is_none(),
            "case \"one past the ceiling\": expected no slot left, got one"
        );
        drop(held);
        let after = TaskSlot::claim();
        assert!(
            after.is_some(),
            "case \"after the tasks ended\": expected the slots to be free again, got none"
        );
    }
}
