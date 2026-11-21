// SPDX-License-Identifier: CC0-1.0

//! `DeleteFramework`: when an audit framework may be removed, and what the
//! refusals say.
//!
//! The rule under test: a framework-scoped mutation is refused 409
//! `ConflictException` for as long as that framework's deployment sits in a
//! non-terminal status, whichever action started that deployment, and the
//! framework is left exactly as it was; once the deployment is terminal the
//! delete removes the record and answers an empty 200, after which every
//! framework-scoped read of the name — the single read and the listing alike —
//! reports the name as one this account holds no framework under (400
//! `ResourceNotFoundException`), as does a name that was never created.
//!
//! Each refusal states the condition with the framework name the request
//! carried and the account the call was made in.
//!
//! The rows of each table differ along what the rule generalises over: which
//! action left the deployment in flight (the create's or an update's), how many
//! controls the framework carries, whether it has a description, and the length
//! of its name. Nothing waits for a fixed time: the settled deployment is waited
//! for by looking again until it is reported, so the suite does not depend on
//! how fast the host runs.
//!
//! Every case drives real HTTP through the router. The request harness is the
//! shared one in `vault_and_backup_job.rs` and the framework helpers are the
//! ones in `framework.rs` and `framework_update.rs`, so there is ONE wire path
//! for this service's suites.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::framework::{
    create_framework, describe_framework, framework_name_of_length, text, DEPLOYMENT_COMPLETED,
    MAX_FRAMEWORK_NAME_LENGTH,
};
use crate::framework_refusals::delete_framework;
use crate::framework_update::{update_framework, DEPLOYMENT_UPDATE_IN_PROGRESS};
use crate::vault_and_backup_job::{call, error_code, router, ACCOUNT};

/// How long the poll below is willing to keep looking for the settled
/// deployment. Generous on purpose: it bounds a stuck emulator, it does not
/// measure the span, which is why nothing here asserts on elapsed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll waits between two looks. A blocking sleep INSIDE the poll
/// loop, for the reason given on `framework::PAST_FRAMEWORK_SETTLE`: the
/// workspace's `tokio` dependency does not enable the `time` feature, and the
/// handlers read the wall clock anyway.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How AWS words "a previous action on this framework has not finished": fixed
/// text around the framework name the request carried and the account the call
/// was made in.
fn concurrent_update_message(name: &str) -> String {
    format!(
        "Operation failed because of a concurrent update for the framework {name} in account \
         {ACCOUNT}. Try again later."
    )
}

/// How AWS words "the framework asked about is not one this account holds".
fn not_found_message(name: &str) -> String {
    format!("Framework {name} not found in account {ACCOUNT}.")
}

/// One `FrameworkControls` entry naming `control_name` — one of the published
/// Backup Audit Manager control identifiers.
fn control(control_name: &str) -> Value {
    json!({"ControlName": control_name})
}

/// Look at the framework until its deployment is reported terminal, or until
/// the generous deadline above passes. Returns the read that reported it.
///
/// `pub(crate)` so a sibling suite in this test binary waits for the settled
/// deployment through the same one poll rather than growing a second copy of it.
pub(crate) async fn settled_framework(router: &MultiServiceRouter, name: &str) -> Value {
    let started = Instant::now();
    loop {
        let (status, read) = describe_framework(router, name).await;
        if status == 200 && text(&read, "DeploymentStatus") == DEPLOYMENT_COMPLETED {
            return read;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "framework {name:?}: the deployment never settled, last read {status} {read:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The frameworks the tables below walk: the rows differ in how many controls
/// the framework carries, in whether it has a description and in the length of
/// its name.
fn framework_cases() -> Vec<(&'static str, String, Value)> {
    let long_name = framework_name_of_length(MAX_FRAMEWORK_NAME_LENGTH);
    let shortest_name = framework_name_of_length(1);
    vec![
        (
            "one control, no description",
            "AuditedFramework".to_string(),
            json!({
                "FrameworkName": "AuditedFramework",
                "FrameworkControls": [control("BACKUP_RECOVERY_POINT_ENCRYPTED")],
            }),
        ),
        (
            "three controls and a description",
            "DescribedFramework".to_string(),
            json!({
                "FrameworkName": "DescribedFramework",
                "FrameworkDescription": "removed by this suite",
                "FrameworkControls": [
                    control("BACKUP_RESOURCES_PROTECTED_BY_BACKUP_PLAN"),
                    control("BACKUP_PLAN_MIN_FREQUENCY_AND_MIN_RETENTION_CHECK"),
                    control("BACKUP_LAST_RECOVERY_POINT_CREATED"),
                ],
            }),
        ),
        (
            "a name at the shortest length the model admits",
            shortest_name.clone(),
            json!({
                "FrameworkName": shortest_name,
                "FrameworkControls": [control("BACKUP_RECOVERY_POINT_MANUAL_DELETION_DISABLED")],
            }),
        ),
        (
            "a name at the longest length the model admits",
            long_name.clone(),
            json!({
                "FrameworkName": long_name,
                "FrameworkControls": [control("BACKUP_RECOVERY_POINT_MINIMUM_RETENTION_CHECK")],
            }),
        ),
    ]
}

/// A delete that arrives while the framework's deployment is still in flight is
/// refused with the conflict the model declares for "a previous action is still
/// running", the refusal names the framework and the account, and the framework
/// is left readable — whether the action in flight is the create's deployment or
/// a later update's.
#[tokio::test]
async fn a_delete_during_any_in_flight_deployment_is_refused_and_changes_nothing() {
    for (case, name, request) in framework_cases() {
        // One router per row: each row's account holds only its own framework.
        let router = router();
        let (status, created) = create_framework(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateFramework 200, got {status} {created:?}"
        );

        // The create's own deployment is the first non-terminal window.
        let (status, errortype, refused) = delete_framework(&router, &name).await;
        assert_eq!(
            (status, errortype.as_str(), error_code(&refused).as_str()),
            (409, "ConflictException", "ConflictException"),
            "case {case:?}: expected 409 ConflictException during the create's deployment, \
             got {status} {errortype} {refused:?}"
        );
        assert_eq!(
            text(&refused, "Message"),
            concurrent_update_message(&name),
            "case {case:?}: the refusal must name the framework and the account, got {refused:?}"
        );
        let (status, kept) = describe_framework(&router, &name).await;
        assert_eq!(
            (status, kept.get("FrameworkArn")),
            (200, created.get("FrameworkArn")),
            "case {case:?}: a refused delete must keep the framework, got {status} {kept:?}"
        );
        assert_eq!(
            kept.get("FrameworkControls"),
            request.get("FrameworkControls"),
            "case {case:?}: a refused delete must keep the controls, got {kept:?}"
        );

        // An update re-deploys the controls, so the SAME refusal holds in the
        // window that update opens — the rule is keyed to the deployment being
        // non-terminal, not to the create.
        settled_framework(&router, &name).await;
        let (status, updated) = update_framework(
            &router,
            &name,
            &json!({"FrameworkDescription": "re-deployed by this suite"}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateFramework 200, got {status} {updated:?}"
        );
        let (status, deploying) = describe_framework(&router, &name).await;
        assert_eq!(
            (status, text(&deploying, "DeploymentStatus").as_str()),
            (200, DEPLOYMENT_UPDATE_IN_PROGRESS),
            "case {case:?}: expected the update's deployment to be in flight, got {deploying:?}"
        );

        let (status, errortype, refused) = delete_framework(&router, &name).await;
        assert_eq!(
            (status, errortype.as_str(), error_code(&refused).as_str()),
            (409, "ConflictException", "ConflictException"),
            "case {case:?}: expected 409 ConflictException during the update's deployment, \
             got {status} {errortype} {refused:?}"
        );
        assert_eq!(
            text(&refused, "Message"),
            concurrent_update_message(&name),
            "case {case:?}: the refusal must name the framework and the account, got {refused:?}"
        );
        let (status, still_there) = describe_framework(&router, &name).await;
        assert_eq!(
            (status, still_there.get("FrameworkArn")),
            (200, created.get("FrameworkArn")),
            "case {case:?}: the twice-refused delete must keep the framework, got {still_there:?}"
        );
    }
}

/// Once the deployment is terminal the delete succeeds with an empty body and
/// the framework is GONE: the single read and the listing both stop reporting
/// it, and a second delete of the name is refused exactly as a name that was
/// never created is.
#[tokio::test]
async fn a_settled_framework_is_deleted_and_then_held_by_nobody() {
    for (case, name, request) in framework_cases() {
        let router = router();
        let (status, created) = create_framework(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateFramework 200, got {status} {created:?}"
        );
        let settled = settled_framework(&router, &name).await;
        assert_eq!(
            text(&settled, "DeploymentStatus"),
            DEPLOYMENT_COMPLETED,
            "case {case:?}: expected a terminal deployment before the delete, got {settled:?}"
        );

        let (status, errortype, deleted) = delete_framework(&router, &name).await;
        assert_eq!(
            (status, errortype.as_str()),
            (200, "<unset>"),
            "case {case:?}: expected DeleteFramework 200, got {status} {errortype} {deleted:?}"
        );
        assert!(
            deleted.is_null() || deleted.as_object().is_some_and(serde_json::Map::is_empty),
            "case {case:?}: DeleteFramework answers an empty body, got {deleted:?}"
        );

        // Both framework-scoped reads agree the account holds nothing under the
        // name now.
        let (status, missing) = describe_framework(&router, &name).await;
        assert_eq!(
            (
                status,
                error_code(&missing).as_str(),
                text(&missing, "Message").as_str()
            ),
            (
                400,
                "ResourceNotFoundException",
                not_found_message(&name).as_str()
            ),
            "case {case:?}: a deleted framework must be unreadable, got {status} {missing:?}"
        );
        let (status, listed) = call(&router, "GET", "/audit/frameworks", None).await;
        assert_eq!(
            (status, listed.get("Frameworks")),
            (200, Some(&json!([]))),
            "case {case:?}: a deleted framework must leave the listing, got {status} {listed:?}"
        );

        // A second delete is the refusal a never-created name gets, and so is a
        // delete of a name this account never held at all.
        let never_created = [
            "NeverCreatedFramework".to_string(),
            name.to_lowercase(),
            framework_name_of_length(MAX_FRAMEWORK_NAME_LENGTH - 1),
        ];
        for probe in std::iter::once(name.clone()).chain(never_created) {
            let (status, errortype, again) = delete_framework(&router, &probe).await;
            assert_eq!(
                (
                    status,
                    errortype.as_str(),
                    text(&again, "Message").as_str()
                ),
                (
                    400,
                    "ResourceNotFoundException",
                    not_found_message(&probe).as_str()
                ),
                "case {case:?}: deleting the unheld name {probe:?} must be refused, \
                 got {status} {again:?}"
            );
        }
    }
}
