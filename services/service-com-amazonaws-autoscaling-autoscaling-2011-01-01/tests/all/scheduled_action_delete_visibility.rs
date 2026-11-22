// SPDX-License-Identifier: CC0-1.0

//! What a DELETE of a scheduled scaling action leaves the group's own read reporting, over
//! the awsQuery wire.
//!
//! The rule under test: a scheduled action lives in ONE collection per Auto Scaling group —
//! the one the puts write and `DescribeScheduledActions` reports — so a delete that names an
//! action the group holds takes that action out of it, through either operation that deletes
//! one, and the read then reports exactly the actions the group still holds, as a list that
//! is present and empty once none remain. A delete that names no action of the group removes
//! nothing and is refused.
//!
//! Each table spans what the rule generalises over — which of the group's actions the delete
//! names, how many it leaves behind, and which operation deletes it — rather than one
//! recorded row; every expected sentence is written out here rather than taken from the
//! crate, and every assertion names its case and the value actually reported.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, new_engine, p, query, router_with, tag_value, tag_values, xml_unescape,
};

/// The launch source every group in this suite is created from.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// Where every group here places the instances it would launch.
const ZONE: (&str, &str) = ("AvailabilityZones.member.1", "us-west-2a");

/// The instant every action of this suite is scheduled for: one far enough ahead that no
/// case depends on when it runs.
const START_TIME: &str = "2035-06-01T00:00:00Z";

/// The wire error code a refused delete carries.
const VALIDATION_ERROR: &str = "ValidationError";

/// The whole message a delete naming an action the group holds none of is refused with: the
/// `<subject> not found - <detail>` shape this service states an absent resource in, with the
/// action name taken from the request.
fn absent_action_message(action: &str) -> String {
    format!("Scheduled action name not found - Scheduled action {action} not found")
}

/// The whole message a delete naming a group the account does not hold is refused with.
fn absent_group_message(group: &str) -> String {
    format!("AutoScalingGroup name not found - AutoScalingGroup {group} not found")
}

/// Which operation a case deletes through: the two that take one named action out of a
/// group's collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    /// `DeleteScheduledAction`, which names the one action it deletes.
    One,
    /// `BatchDeleteScheduledAction`, naming that single action.
    Batch,
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

/// Put the scheduled action `action` on `group`, timed and setting one size.
async fn given_action(router: &MultiServiceRouter, group: &str, action: &str) {
    let params = vec![
        p("AutoScalingGroupName", group),
        p("ScheduledActionName", action),
        p("StartTime", START_TIME),
        p("MinSize", "1"),
    ];
    let (status, body) = query(
        router,
        "PutScheduledUpdateGroupAction",
        &borrowed(&params),
    )
    .await;
    assert_eq!(
        status, 200,
        "putting {action:?} on {group:?} must succeed, got {status} with {body:?}"
    );
}

/// Post one delete of the single action `action` from `group`, through `via`, and answer its
/// status, wire error code and message.
async fn delete(
    router: &MultiServiceRouter,
    group: &str,
    action: &str,
    via: Via,
) -> (u16, String, String) {
    let (action_name_member, operation) = match via {
        Via::One => ("ScheduledActionName", "DeleteScheduledAction"),
        Via::Batch => (
            "ScheduledActionNames.member.1",
            "BatchDeleteScheduledAction",
        ),
    };
    let (status, body) = query(
        router,
        operation,
        &[
            ("AutoScalingGroupName", group),
            (action_name_member, action),
        ],
    )
    .await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// The names of the actions the group still holds, in the order the read reports them, and
/// whether the read reported the list element at all.
async fn held(router: &MultiServiceRouter, case: &str, group: &str) -> (Vec<String>, bool) {
    let (status, body) = query(
        router,
        "DescribeScheduledActions",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read must succeed, got {status} with {body:?}"
    );
    // The collection is reported as a LIST — present, and empty for a group holding no
    // action — so "holds nothing" and "this service does not report them" stay apart.
    let reported = body.contains("<ScheduledUpdateGroupActions");
    let names = tag_values(&body, "ScheduledActionName");
    (names, reported)
}

/// RULE: a delete of an action the group holds takes it out of the collection the read
/// reports, whichever operation deletes it and whatever the group holds beside it.
#[tokio::test]
async fn a_delete_takes_the_action_out_of_what_the_read_reports() {
    // Each row: the case, the actions the group is given, the one the delete names, and the
    // actions the read must then report (a group's actions are reported in action-name
    // order).
    let rows: Vec<(&str, Vec<&str>, &str, Vec<&str>)> = vec![
        ("the group's only action", vec!["alpha"], "alpha", vec![]),
        (
            "the first of three",
            vec!["alpha", "beta", "gamma"],
            "alpha",
            vec!["beta", "gamma"],
        ),
        (
            "the last of three",
            vec!["alpha", "beta", "gamma"],
            "gamma",
            vec!["alpha", "beta"],
        ),
        (
            "the middle of three",
            vec!["alpha", "beta", "gamma"],
            "beta",
            vec!["alpha", "gamma"],
        ),
        (
            "one of two, leaving one",
            vec!["only-one", "the-other"],
            "the-other",
            vec!["only-one"],
        ),
    ];

    for (row, (case, given, deleted, remaining)) in rows.into_iter().enumerate() {
        for via in [Via::One, Via::Batch] {
            let case = format!("{case} deleted through {via:?}");
            let group = format!("asg-delete-{row}-{via:?}");
            let router = given_group(&group).await;
            for action in &given {
                given_action(&router, &group, action).await;
            }
            // The action is there to begin with, so what the read reports afterwards is the
            // delete's own doing.
            let (before, _) = held(&router, &case, &group).await;
            assert!(
                before.iter().any(|name| name == deleted),
                "case {case:?}: the action to delete must be reported first, got {before:?}"
            );

            let (status, code, message) = delete(&router, &group, deleted, via).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the delete must be served, got {status} {code:?} {message:?}"
            );
            let (after, reported) = held(&router, &case, &group).await;
            assert_eq!(
                after, remaining,
                "case {case:?}: expected the read to report {remaining:?}, got {after:?}"
            );
            assert!(
                reported,
                "case {case:?}: expected the read to report the collection as a list even when \
                 the group holds none, got no list element"
            );
        }
    }
}

/// RULE, continued: a delete that names no action of the group deletes nothing — refused for
/// the action when the group holds none of that name, and for the group when the account
/// holds no such group.
#[tokio::test]
async fn a_delete_naming_no_action_of_the_group_deletes_nothing() {
    let group = "asg-delete-refusals";
    let router = given_group(group).await;
    for action in ["alpha", "beta"] {
        given_action(&router, group, action).await;
    }

    // Each row: the case, the group the delete names, the action it names, and the whole
    // sentence it must be refused with.
    let rows: Vec<(&str, &str, &str, String)> = vec![
        (
            "a name the group holds no action under",
            group,
            "nonesuch",
            absent_action_message("nonesuch"),
        ),
        (
            "a name that differs from a held one only in case",
            group,
            "Alpha",
            absent_action_message("Alpha"),
        ),
        (
            "a group the account does not hold",
            "asg-delete-absent",
            "alpha",
            absent_group_message("asg-delete-absent"),
        ),
    ];

    for (case, named_group, action, expected) in rows {
        let (status, code, message) = delete(&router, named_group, action, Via::One).await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, VALIDATION_ERROR, expected.as_str()),
            "case {case:?}: expected the delete to be refused, got {status} {code:?} {message:?}"
        );
        let (names, _) = held(&router, case, group).await;
        assert_eq!(
            names,
            vec!["alpha".to_string(), "beta".to_string()],
            "case {case:?}: expected a refused delete to delete nothing, got {names:?}"
        );
    }

    // Deleting an action the group holds and then deleting it AGAIN: the second call names an
    // action the group no longer holds, which is the refusal above.
    let (status, code, message) = delete(&router, group, "alpha", Via::One).await;
    assert_eq!(
        status, 200,
        "the first delete of {:?} must be served, got {status} {code:?} {message:?}",
        "alpha"
    );
    let (status, code, message) = delete(&router, group, "alpha", Via::One).await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (400, VALIDATION_ERROR, absent_action_message("alpha").as_str()),
        "deleting the same action again must be refused for the action, got {status} {code:?} \
         {message:?}"
    );
    let (names, _) = held(&router, "after the repeated delete", group).await;
    assert_eq!(
        names,
        vec!["beta".to_string()],
        "expected the group to hold only the action no delete named, got {names:?}"
    );
}
