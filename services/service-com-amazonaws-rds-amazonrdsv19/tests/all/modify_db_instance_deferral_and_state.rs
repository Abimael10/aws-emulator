// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHEN a `ModifyDBInstance` lands, and for how it words the refusal of
//! an instance that cannot take one.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST carrying the SigV4 credential scope the router keys on, so the assertions read the
//! XML the emulator itself answered with.
//!
//! The rules under test:
//!
//! * `ApplyImmediately` decides WHEN the settings a modification stages land, and the LAST
//!   request's answer is the staged record's: enabled, "the modifications in this request and
//!   any pending modifications are asynchronously applied as soon as possible, regardless of
//!   the PreferredMaintenanceWindow setting"; disabled, "changes to the DB instance are
//!   applied during the next maintenance window". So a deferred modification is reported in
//!   `PendingModifiedValues` — "changes that will be applied during the next maintenance
//!   window" — and STAYS there until that window comes round, a later immediate modification
//!   supersedes it or the reboot such a change waits for applies it; an instant an earlier
//!   request asked for does not apply it.
//! * the refusal of an instance in a status a modification cannot be taken in is worded by
//!   WHAT that status is: one the instance leaves on its own states the state the call needs,
//!   one only a caller brings it out of states what to do about it, and a terminal one names
//!   the status itself.
//!
//! Nothing here waits out a span: every state a transition this service owes leads to is
//! reached by polling a REPORTING call through `lifecycle_poll_loops::poll_until`.

use service_com_amazonaws_rds_amazonrdsv19::state::{
    modify_db_instance_state_message, DB_INSTANCE_STATUS_CREATING, DB_INSTANCE_STATUS_STOPPED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_automated_backups::{available_instance, describe_instance};
use crate::db_instances::{
    call, element_is_empty, emulator, expect_error, expect_ok, optional_text, subtrees, text,
    the_instance,
};
use crate::lifecycle_poll_loops::poll_until;
use crate::pending_modified_values::window_days_ahead;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The class every instance below is created with, and the one a modification moves it to.
const CLASS: &str = "db.t3.micro";
const LARGER_CLASS: &str = "db.t3.small";

/// The storage every instance below is created with, and the size a modification grows it
/// to.
const STORAGE: &str = "20";
const LARGER_STORAGE: &str = "40";

/// The `awsQueryError` code of `InvalidDBInstanceStateFault`.
const INVALID_DB_INSTANCE_STATE: &str = "InvalidDBInstanceState";

/// What `instance` reports as pending, or `None` when it reports no pending change.
///
/// The element itself is reported on every instance envelope — "specific changes are
/// identified by subelements", and an instance with nothing pending carries it with no
/// subelement inside — so an EMPTY element is what "nothing is pending" reads as here.
fn pending(instance: &str) -> Option<String> {
    subtrees(instance, "PendingModifiedValues")
        .into_iter()
        .find(|reported| !element_is_empty(reported, "PendingModifiedValues"))
}

/// The one `<DBInstance>` a describe of `identifier` reports.
async fn described(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let (_, body) = describe_instance(router, identifier).await;
    the_instance(case, &body)
}

/// RULE: a modification left for the maintenance window stays pending — an instant an
/// EARLIER request asked for its own settings at does not apply it — and the reboot such a
/// change waits for is what applies it.
#[tokio::test]
async fn a_deferred_modification_outlives_the_instant_an_earlier_request_asked_for() {
    let router = emulator();
    let case = "a deferred modification after an immediate one";
    let identifier = "emu-defer-after-now";
    // A maintenance window whose next occurrence is DAYS away, so the window itself cannot
    // apply the staged change while this test runs.
    available_instance(
        &router,
        case,
        identifier,
        "postgres",
        params!["PreferredMaintenanceWindow" => window_days_ahead()].as_ref(),
    )
    .await;

    // First, a modification the caller asked for AS SOON AS POSSIBLE: the instance is
    // applying it, so it reports `modifying` and the requested storage as pending.
    let applied_now = expect_ok(
        case,
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "ApplyImmediately" => true,
                "AllocatedStorage" => LARGER_STORAGE,
            ],
        )
        .await,
    );
    let status = optional_text(&applied_now, "DBInstanceStatus");
    assert_eq!(
        status.as_deref(),
        Some("modifying"),
        "case {case:?}: a modification being applied reports modifying, got {status:?}"
    );

    // Then, while that one is still being applied, a modification the caller left for the
    // maintenance window. It is staged beside the first, and the window is what it waits
    // for: the instant the first request asked for is not its own.
    let deferred = expect_ok(
        case,
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "ApplyImmediately" => false,
                "DBInstanceClass" => LARGER_CLASS,
            ],
        )
        .await,
    );
    let staged = pending(&deferred).unwrap_or_else(|| {
        panic!("case {case:?}: a deferred modification reports PendingModifiedValues, got {deferred}")
    });
    assert_eq!(
        optional_text(&staged, "DBInstanceClass").as_deref(),
        Some(LARGER_CLASS),
        "case {case:?}: expected the requested class staged, got {staged}"
    );

    // The instance comes back out of `modifying` on its own, which is the transition this
    // service owes for the first request: polled for, never waited out.
    let (settled, _) = poll_until(
        case,
        "the instance available again",
        || describe_instance(&router, identifier),
        |status, body| {
            status == 200
                && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
        },
    )
    .await;
    let instance = the_instance(case, &settled);
    // And the deferred settings are STILL pending: nothing applied them, so the instance is
    // still running under the values it had.
    let staged = pending(&instance).unwrap_or_else(|| {
        panic!(
            "case {case:?}: the settings left for the window stay pending, got {instance}"
        )
    });
    for (member, value) in [
        ("DBInstanceClass", LARGER_CLASS),
        ("AllocatedStorage", LARGER_STORAGE),
    ] {
        assert_eq!(
            optional_text(&staged, member).as_deref(),
            Some(value),
            "case {case:?}: expected {member} to stay staged as {value:?}, got {staged}"
        );
    }
    for (member, value) in [("DBInstanceClass", CLASS), ("AllocatedStorage", STORAGE)] {
        assert_eq!(
            optional_text(&instance, member).as_deref(),
            Some(value),
            "case {case:?}: expected the live {member} to stay {value:?}, got {instance}"
        );
    }

    // The staged settings are real: the reboot a deferred change waits for applies them —
    // "some parameter changes ... are applied on the next call to RebootDBInstance".
    expect_ok(
        case,
        call(
            &router,
            "RebootDBInstance",
            &params!["DBInstanceIdentifier" => identifier],
        )
        .await,
    );
    let (rebooted, _) = poll_until(
        case,
        "the instance available with nothing pending",
        || describe_instance(&router, identifier),
        |status, body| {
            status == 200
                && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
                && subtrees(body, "DBInstance")
                    .into_iter()
                    .next()
                    .is_some_and(|instance| pending(&instance).is_none())
        },
    )
    .await;
    let instance = the_instance(case, &rebooted);
    for (member, value) in [
        ("DBInstanceClass", LARGER_CLASS),
        ("AllocatedStorage", LARGER_STORAGE),
    ] {
        assert_eq!(
            optional_text(&instance, member).as_deref(),
            Some(value),
            "case {case:?}: expected the reboot to apply {member} as {value:?}, got {instance}"
        );
    }
}

/// RULE: the refusal of a modification is worded by the blocking STATUS CLASS — the state
/// the call needs for a status the instance leaves on its own, the instruction out of the
/// status only a caller ends, and the status itself for a terminal one.
#[tokio::test]
async fn the_state_refusal_is_worded_by_what_the_blocking_status_is() {
    let router = emulator();

    // A status the instance leaves on its own: the create is still running.
    let still_creating = "emu-modstate-creating";
    expect_ok(
        "an instance still coming up",
        call(
            &router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => still_creating,
                "Engine" => "postgres",
                "DBInstanceClass" => CLASS,
                "AllocatedStorage" => STORAGE,
                "MasterUsername" => "emuadmin",
                "MasterUserPassword" => "emu-Secret-99",
            ],
        )
        .await,
    );
    let refused = expect_error(
        "an instance still coming up",
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => still_creating,
                "BackupRetentionPeriod" => 7,
            ],
        )
        .await,
        400,
        INVALID_DB_INSTANCE_STATE,
    );
    assert_eq!(
        text(&refused, "Message"),
        modify_db_instance_state_message(DB_INSTANCE_STATUS_CREATING),
        "a status the instance leaves on its own states the state a modification needs, got \
         {refused}"
    );

    // And the status only a CALLER ends: a stopped instance stays stopped, so the sentence
    // is the instruction out of it rather than a state to poll for.
    let stopped = "emu-modstate-stopped";
    available_instance(&router, "a stopped instance", stopped, "postgres", &[]).await;
    expect_ok(
        "a stopped instance",
        call(
            &router,
            "StopDBInstance",
            &params!["DBInstanceIdentifier" => stopped],
        )
        .await,
    );
    poll_until(
        "a stopped instance",
        "the instance reaching stopped",
        || describe_instance(&router, stopped),
        |status, body| {
            status == 200
                && optional_text(body, "DBInstanceStatus").as_deref()
                    == Some(DB_INSTANCE_STATUS_STOPPED)
        },
    )
    .await;
    let refused = expect_error(
        "a stopped instance",
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => stopped,
                "BackupRetentionPeriod" => 7,
            ],
        )
        .await,
        400,
        INVALID_DB_INSTANCE_STATE,
    );
    let message = text(&refused, "Message");
    assert_eq!(
        message,
        "You can't modify a stopped DB instance. Start the DB instance, and then modify it.",
        "a stopped instance is told to start first, got {message:?}"
    );
    assert_eq!(
        message,
        modify_db_instance_state_message(DB_INSTANCE_STATUS_STOPPED),
        "the wire sentence is the one this service words for a stopped instance, got \
         {message:?}"
    );
    // The refused call wrote nothing: the instance is still stopped and has nothing pending.
    let instance = described(&router, "a stopped instance", stopped).await;
    assert_eq!(
        optional_text(&instance, "DBInstanceStatus").as_deref(),
        Some(DB_INSTANCE_STATUS_STOPPED),
        "a refused modification leaves the instance where it was, got {instance}"
    );
    assert!(
        pending(&instance).is_none(),
        "a refused modification stages nothing, got {instance}"
    );
}
