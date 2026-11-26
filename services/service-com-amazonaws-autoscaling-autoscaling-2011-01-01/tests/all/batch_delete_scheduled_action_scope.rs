// SPDX-License-Identifier: CC0-1.0

//! Which collection `BatchDeleteScheduledAction` takes a name out of, over the awsQuery wire.
//!
//! The rule under test: a scheduled action is a resource OF the Auto Scaling group it was put
//! on, so one call resolves every name it states only inside the group that call names. From
//! that one rule follow all three things asserted here:
//!
//! 1. SCOPE — a name only another group holds names no action of the group called, so it is
//!    reported under `FailedScheduledActions` and the group that does hold it keeps it; a name
//!    both groups hold is taken out of the group called and left in the other.
//! 2. IDEMPOTENCE — a delete is a change of that one collection, not a counter: once a name is
//!    gone, a later call stating it reports it and takes nothing else with it.
//! 3. ORDER — the names a call could not delete are reported in the order the call states
//!    them, not in an order this service chose.
//!
//! Each row of the table differs in which group the call names and which of the two groups'
//! names it states; every expected sentence is written out in this suite rather than taken
//! from the crate, and every assertion names its case and the value actually reported.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, list_members, new_engine, p, query, router_with, tag_value, xml_unescape,
};

/// The launch source both groups in this suite are created from.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// Where both groups here place the instances they would launch.
const ZONE: (&str, &str) = ("AvailabilityZones.member.1", "us-west-2a");

/// The two groups this suite holds actions on.
const GROUP_A: &str = "scope-group-a";
const GROUP_B: &str = "scope-group-b";

/// The wire error code a name the group called holds no action under is reported with: the code
/// this service answers a request about a resource it has no record of with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The whole message such a name is reported with: fixed text naming the subject that was
/// not found, with the name itself carried by the entry's own `ScheduledActionName` member.
/// Written out here so the assertion does not take the handler's own word for it.
const ABSENT_ACTION_MESSAGE: &str = "Scheduled action name not found";

/// Create the group `name` on `router`, with nothing to launch.
async fn create_group(router: &MultiServiceRouter, name: &str) {
    let (status, body) = query(
        router,
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
        "putting {actions:?} on {group:?} must succeed, got {status} with {body:?}"
    );
    assert!(
        list_members(&body, "FailedScheduledUpdateGroupActions").is_empty(),
        "putting {actions:?} on {group:?} must store every one of them, got {body}"
    );
}

/// Post one `BatchDeleteScheduledAction` and answer its status, the entries it reported as
/// failed in the order it reported them, and its body.
async fn batch_delete(
    router: &MultiServiceRouter,
    group: &str,
    names: &[&str],
) -> (u16, Vec<(String, String, String)>, String) {
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
    (status, failed, body)
}

/// The names of the actions `group` still holds, in the order the read reports them.
async fn held_names(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeScheduledActions",
        &[("AutoScalingGroupName", group), ("MaxRecords", "100")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {group:?} must succeed, got {status} with {body:?}"
    );
    list_members(&body, "ScheduledUpdateGroupActions")
        .into_iter()
        .map(|action| tag_value(action, "ScheduledActionName"))
        .collect()
}

/// One row: the case, the group the call names, the names it states, the names it must report
/// as failed in that order, then what each group must hold afterwards.
type ScopeCase<'a> = (
    &'a str,
    &'a str,
    Vec<&'a str>,
    Vec<&'a str>,
    Vec<&'a str>,
    Vec<&'a str>,
);

/// A call deletes only out of the group it names, reports every name that group holds no
/// action under in request order, and a repeat of a served call reports what it already took.
#[tokio::test]
async fn one_call_deletes_only_out_of_the_group_it_names() {
    let engine = new_engine();
    let router = router_with(engine);
    create_group(&router, GROUP_A).await;
    create_group(&router, GROUP_B).await;
    // Both groups hold "shared"; each also holds a name of its own, so a call can state a name
    // the group called does not hold while another group does.
    given_actions(&router, GROUP_A, &["a-only", "shared"]).await;
    given_actions(&router, GROUP_B, &["b-only", "shared"]).await;

    // The read reports a group's actions in action-name order, which is the order every
    // expectation below is written in.
    let rows: Vec<ScopeCase<'_>> = vec![
        (
            "a name only the other group holds",
            GROUP_A,
            vec!["b-only"],
            vec!["b-only"],
            vec!["a-only", "shared"],
            vec!["b-only", "shared"],
        ),
        (
            "two names the group called holds no action under, in the order stated",
            GROUP_A,
            vec!["zz-absent", "aa-absent"],
            vec!["zz-absent", "aa-absent"],
            vec!["a-only", "shared"],
            vec!["b-only", "shared"],
        ),
        (
            "a name both groups hold is taken out of the group called only",
            GROUP_A,
            vec!["shared"],
            vec![],
            vec!["a-only"],
            vec!["b-only", "shared"],
        ),
        (
            "the same call again, after it was served",
            GROUP_A,
            vec!["shared"],
            vec!["shared"],
            vec!["a-only"],
            vec!["b-only", "shared"],
        ),
        (
            "names of both groups in one call against the other group",
            GROUP_B,
            vec!["a-only", "shared", "b-only"],
            vec!["a-only"],
            vec!["a-only"],
            vec![],
        ),
    ];

    for (case, group, names, expected_failed, expected_a, expected_b) in rows {
        let (status, failed, body) = batch_delete(&router, group, &names).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the call must be served, got {status} with {body}"
        );
        let expected: Vec<(String, String, String)> = expected_failed
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    VALIDATION_ERROR.to_string(),
                    ABSENT_ACTION_MESSAGE.to_string(),
                )
            })
            .collect();
        assert_eq!(
            failed, expected,
            "case {case:?}: expected one entry per name {group:?} holds no action under, in \
             request order, got {failed:?} in {body}"
        );
        let held_a = held_names(&router, case, GROUP_A).await;
        assert_eq!(
            held_a, expected_a,
            "case {case:?}: expected {GROUP_A:?} to hold {expected_a:?}, got {held_a:?}"
        );
        let held_b = held_names(&router, case, GROUP_B).await;
        assert_eq!(
            held_b, expected_b,
            "case {case:?}: expected {GROUP_B:?} to hold {expected_b:?}, got {held_b:?}"
        );
    }
}
