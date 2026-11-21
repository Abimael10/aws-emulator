// SPDX-License-Identifier: CC0-1.0

//! Deleting SEVERAL scheduled scaling actions of one Auto Scaling group in a single call,
//! over the awsQuery wire: what `BatchDeleteScheduledAction` takes out of the collection
//! `DescribeScheduledActions` reports from, and what it reports about a name it could not
//! delete.
//!
//! The rules under test:
//!
//! 1. One call deletes every action it names, out of the same collection both writers of a
//!    scheduled action write, and the read then reports only the actions the group still
//!    holds; a call that could delete every name it stated reports no failed action at all.
//! 2. Each name of the call is a resource of its own: a name the group holds no action under
//!    could not be deleted, so it is reported under `FailedScheduledActions` ("the names of
//!    the scheduled actions that could not be deleted, including an error message") under the
//!    wire error code of its class, and the names stated beside it are still deleted. A name
//!    the call states TWICE names one action, which the call deletes once.
//! 3. "The names of the scheduled actions to delete. The maximum number allowed is 50."
//!    (`BatchDeleteScheduledActionType$ScheduledActionNames`): a call stating more is refused
//!    for the member and deletes nothing.
//! 4. Every name of the call belongs to the one group it names, so a call naming a group the
//!    account does not hold is refused for the group and deletes nothing.
//!
//! Each table spans what its rule generalises over (how many names one call states, and which
//! of them the group holds) rather than one recorded row, every expected sentence is written
//! out here rather than taken from the crate, and every assertion names its case and the
//! value actually reported.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, list_members, new_engine, p, query, router_with, tag_value, xml_unescape,
};

/// The launch source every group in this suite is created from.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// Where every group here places the instances it would launch.
const ZONE: (&str, &str) = ("AvailabilityZones.member.1", "us-west-2a");

/// The most names ONE call may state, from the member's own documentation. Written out here
/// rather than taken from the crate, so the assertion does not take the handler's own word
/// for it.
const MAX_SCHEDULED_ACTION_NAMES_PER_CALL: usize = 50;

/// The wire error code a name the group holds no action under is reported under: the code this
/// service answers a request about a resource it has no record of with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The whole message a name the group holds no action under is reported with: the
/// `<subject> not found - <detail>` shape this service states an absent resource in.
fn absent_action_message(action: &str) -> String {
    format!("Scheduled action name not found - Scheduled action {action} not found")
}

/// The whole message a call naming a group the account does not hold is refused with.
fn absent_group_message(name: &str) -> String {
    format!("AutoScalingGroup name not found - AutoScalingGroup {name} not found")
}

/// The whole message a call stating more names than the bound is refused with: the sentence
/// this service states for a list member past the bound it has to satisfy, naming the member
/// as the wire names it.
fn too_many_names_message(count: usize) -> String {
    format!(
        "1 validation error detected: Value with {count} items at 'scheduledActionNames' \
         failed to satisfy constraint: Member must have length less than or equal to \
         {MAX_SCHEDULED_ACTION_NAMES_PER_CALL}"
    )
}

/// A router carrying Amazon EC2 Auto Scaling on one engine, with the group `name` created on
/// it and nothing to launch.
async fn given_group(name: &str) -> MultiServiceRouter {
    let router = router_with(new_engine());
    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            LAUNCH_SOURCE,
            ("MinSize", "0"),
            ("MaxSize", "3"),
            ("DesiredCapacity", "0"),
            ZONE,
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
    router
}

/// Put the scheduled actions `actions` on `group`, each timed and setting one size.
async fn given_actions(router: &MultiServiceRouter, group: &str, actions: &[&str]) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, action) in actions.iter().enumerate() {
        let prefix = format!("ScheduledUpdateGroupActions.member.{}", index + 1);
        params.push(p(&format!("{prefix}.ScheduledActionName"), *action));
        params.push(p(&format!("{prefix}.StartTime"), "2030-06-01T12:00:00Z"));
        params.push(p(&format!("{prefix}.MinSize"), "1"));
    }
    let (status, body) = query(
        router,
        "BatchPutScheduledUpdateGroupAction",
        &borrowed(&params),
    )
    .await;
    assert_eq!(
        status, 200,
        "putting {actions:?} must succeed, got {status} with {body:?}"
    );
    assert!(
        list_members(&body, "FailedScheduledUpdateGroupActions").is_empty(),
        "putting {actions:?} must store every one of them, got {body}"
    );
}

/// Post one `BatchDeleteScheduledAction` and answer its status, wire code, message, the
/// entries it reported as failed, and its body.
async fn batch_delete(
    router: &MultiServiceRouter,
    group: &str,
    names: &[&str],
) -> (u16, String, String, Vec<(String, String, String)>, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, name) in names.iter().enumerate() {
        params.push(p(
            &format!("ScheduledActionNames.member.{}", index + 1),
            *name,
        ));
    }
    let (status, body) = query(router, "BatchDeleteScheduledAction", &borrowed(&params)).await;
    let failed = list_members(&body, "FailedScheduledActions")
        .into_iter()
        .map(|block| {
            (
                tag_value(block, "ScheduledActionName"),
                tag_value(block, "ErrorCode"),
                xml_unescape(&tag_value(block, "ErrorMessage")),
            )
        })
        .collect();
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
        failed,
        body,
    )
}

/// The names of the actions the group still holds, in the order the read reports them.
async fn held_names(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeScheduledActions",
        &[("AutoScalingGroupName", group), ("MaxRecords", "100")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read must succeed, got {status} with {body:?}"
    );
    list_members(&body, "ScheduledUpdateGroupActions")
        .into_iter()
        .map(|action| tag_value(action, "ScheduledActionName"))
        .collect()
}

/// One call of rule 1: the case, the names the call states, the names it must report as
/// failed, and the actions the group must still hold afterwards.
type DeleteCase<'a> = (&'a str, Vec<&'a str>, Vec<&'a str>, Vec<&'a str>);

/// RULES 1 and 2: one call deletes every name the group holds an action under and reports
/// every name it does not, whatever mixture of the two it states.
#[tokio::test]
async fn one_call_deletes_the_names_the_group_holds_and_reports_the_rest() {
    // Each row: the case, the names the call states, the entries it must report as failed,
    // and the actions the group must still hold afterwards ([`DeleteCase`]).
    let rows: Vec<DeleteCase<'_>> = vec![
        (
            "one name the group holds",
            vec!["alpha"],
            vec![],
            // The read reports a group's actions in action-name order, which is the order
            // every expectation here is written in.
            vec!["beta", "delta", "gamma"],
        ),
        (
            "two names the group holds",
            vec!["beta", "gamma"],
            vec![],
            vec!["delta"],
        ),
        (
            "a name the group holds beside one it does not",
            vec!["delta", "nonesuch"],
            vec!["nonesuch"],
            vec![],
        ),
        (
            "only names the group does not hold",
            vec!["nonesuch", "never-stored"],
            vec!["nonesuch", "never-stored"],
            vec![],
        ),
    ];

    let group = "batch-delete";
    let router = given_group(group).await;
    given_actions(&router, group, &["alpha", "beta", "gamma", "delta"]).await;

    for (case, names, expected_failed, expected_held) in rows {
        let (status, code, message, failed, body) = batch_delete(&router, group, &names).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the call must be served, got {status} {code:?} {message:?}"
        );
        let expected: Vec<(String, String, String)> = expected_failed
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    VALIDATION_ERROR.to_string(),
                    absent_action_message(name),
                )
            })
            .collect();
        assert_eq!(
            failed, expected,
            "case {case:?}: expected one entry per name the call could not delete, got \
             {failed:?} in {body}"
        );
        let held = held_names(&router, case, group).await;
        assert_eq!(
            held, expected_held,
            "case {case:?}: expected the group to hold {expected_held:?}, got {held:?}"
        );
    }

    // A name the call states TWICE names one action, which the call deletes once.
    given_actions(&router, group, &["twice"]).await;
    let (status, code, message, failed, body) =
        batch_delete(&router, group, &["twice", "twice"]).await;
    assert_eq!(
        (status, failed.as_slice()),
        (200, [].as_slice()),
        "a name stated twice must be deleted once and reported for neither, got {status} \
         {code:?} {message:?} with {body}"
    );
    let held = held_names(&router, "after the name stated twice", group).await;
    assert!(
        held.is_empty(),
        "expected the action to be gone, got {held:?}"
    );
}

/// RULE 3: a call may state at most 50 names — that many is served, and one more is refused
/// for the member and deletes nothing.
#[tokio::test]
async fn the_most_names_one_call_may_state() {
    let group = "batch-delete-bound";
    let router = given_group(group).await;
    let held: Vec<String> = (0..MAX_SCHEDULED_ACTION_NAMES_PER_CALL)
        .map(|index| format!("held-{index:03}"))
        .collect();
    let borrowed_held: Vec<&str> = held.iter().map(String::as_str).collect();
    given_actions(&router, group, &borrowed_held).await;

    // One past the bound FIRST, so what the group holds afterwards shows the refused call
    // deleted nothing.
    let over: Vec<String> = (0..=MAX_SCHEDULED_ACTION_NAMES_PER_CALL)
        .map(|index| format!("held-{index:03}"))
        .collect();
    let borrowed_over: Vec<&str> = over.iter().map(String::as_str).collect();
    let (status, code, message, _, body) = batch_delete(&router, group, &borrowed_over).await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (
            400,
            VALIDATION_ERROR,
            too_many_names_message(MAX_SCHEDULED_ACTION_NAMES_PER_CALL + 1).as_str()
        ),
        "a call stating {} names must be refused for the member, got {status} {code:?} \
         {message:?} with {body}",
        borrowed_over.len()
    );
    let still_held = held_names(&router, "after the refused call", group).await;
    assert_eq!(
        still_held.len(),
        MAX_SCHEDULED_ACTION_NAMES_PER_CALL,
        "expected a refused call to delete nothing, got {} actions",
        still_held.len()
    );

    // Exactly the bound is served, and takes every action with it.
    let (status, code, message, failed, body) = batch_delete(&router, group, &borrowed_held).await;
    assert_eq!(
        (status, failed.as_slice()),
        (200, [].as_slice()),
        "a call stating the bound must be served whole, got {status} {code:?} {message:?} \
         with {body}"
    );
    let still_held = held_names(&router, "after the call at the bound", group).await;
    assert!(
        still_held.is_empty(),
        "expected the call at the bound to delete every action, got {still_held:?}"
    );
}

/// RULE 4: a call naming a group the account does not hold is refused for the group, and
/// deletes nothing.
#[tokio::test]
async fn a_call_naming_no_stored_group_is_refused_for_the_group() {
    let group = "batch-delete-held";
    let router = given_group(group).await;
    given_actions(&router, group, &["alpha"]).await;

    for absent in ["batch-delete-absent", "batch-delete-held-2"] {
        let (status, code, message, _, body) = batch_delete(&router, absent, &["alpha"]).await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (
                400,
                VALIDATION_ERROR,
                absent_group_message(absent).as_str()
            ),
            "a call against the absent group {absent:?} must be refused for the group, got \
             {status} {code:?} {message:?} with {body}"
        );
    }
    let held = held_names(&router, "the group the account does hold", group).await;
    assert_eq!(
        held,
        vec!["alpha".to_string()],
        "expected a refused call to delete nothing, got {held:?}"
    );
}
