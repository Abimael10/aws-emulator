// SPDX-License-Identifier: CC0-1.0

//! A group's scheduled scaling actions, over the WHOLE mounted service set — the one
//! `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **an Auto Scaling group built on a launch template of the account's
//! own Amazon EC2 holds the scheduled actions `PutScheduledUpdateGroupAction` states on it —
//! each reported by `DescribeScheduledActions` with the instant it next runs, which for a
//! purely recurring action is the next occurrence of its crontab expression — and deleting
//! that group takes them with it.**
//!
//! Two services have to agree for that to hold: Amazon EC2 holds the launch template the
//! group is created from, and the group's own create resolves it, so the path from the
//! template to the scheduled actions of the group built on it is only reachable with both
//! mounted. The RULES of a scheduled action — the upsert, the projection, the selection, the
//! refusals — are pinned over tables by the service's own suite,
//! `services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/tests/all/scaling_policies/scheduled_actions.rs`;
//! what only this suite reaches is the same path over the production service set.
//!
//! The rows differ along what the rule generalises over: how the action is timed (an instant
//! it states, a recurrence it is read in UTC by) and the Region the whole wiring lives in.

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::router;
use crate::query_wire::{elements, form_encode, members_of, opt_element};

/// The launch template every group here launches from.
const TEMPLATE: &str = "router-scheduled-action-template";

/// One form-wire call against the service whose signing name is `signing_name`, which must
/// answer 200.
async fn form_call(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in params {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {action}, got {status} with \
         {text}"
    );
    text
}

/// Each scheduled action one `DescribeScheduledActions` answer reports, as its own member
/// block.
fn reported_actions(body: &str) -> Vec<&str> {
    let Some(open) = body.find("<ScheduledUpdateGroupActions") else {
        return Vec::new();
    };
    let Some((_, list)) = body[open..].split_once('>') else {
        return Vec::new();
    };
    members_of(
        list.split("</ScheduledUpdateGroupActions>")
            .next()
            .unwrap_or_default(),
    )
}

/// The block of the action named `name`, or `None` when the answer reports no such action.
fn action_named<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    reported_actions(body).into_iter().find(|action| {
        elements(action, "ScheduledActionName")
            .first()
            .is_some_and(|reported| reported == name)
    })
}

/// One leg this suite drives: the case, the Region the wiring lives in, the action's name,
/// the definition the put states, and the UTC time of day the action's next run falls at.
type Leg<'a> = (&'a str, &'a str, &'a str, &'a [(&'a str, &'a str)], &'a str);

#[tokio::test]
async fn a_group_built_on_an_ec2_launch_template_holds_its_scheduled_actions() {
    // (case, the Region the wiring lives in, the action's name, the definition the put
    //  states, the UTC time of day the action's next run falls at)
    let rows: &[Leg<'_>] = &[
        (
            "an action timed by the instant it states",
            "us-west-2",
            "one-off",
            &[
                ("StartTime", "2030-06-01T12:00:00Z"),
                ("DesiredCapacity", "2"),
            ],
            "2030-06-01T12:00:00Z",
        ),
        (
            "a recurring action, in another Region",
            "eu-west-1",
            "recurring",
            &[("Recurrence", "17 4 * * *"), ("MaxSize", "3")],
            "T04:17:00Z",
        ),
    ];

    for (case, region, name, definition, runs_at) in rows {
        let router = router();

        // What the group launches from: a launch template of the account's own Amazon EC2.
        form_call(
            &router,
            case,
            region,
            "ec2",
            "2016-11-15",
            "CreateLaunchTemplate",
            &[
                ("LaunchTemplateName", TEMPLATE),
                ("LaunchTemplateData.ImageId", "ami-0abcdef1234567890"),
                ("LaunchTemplateData.InstanceType", "t3.micro"),
            ],
        )
        .await;

        let group = "scheduled-action-group";
        let zone = format!("{region}a");
        form_call(
            &router,
            case,
            region,
            "autoscaling",
            "2011-01-01",
            "CreateAutoScalingGroup",
            &[
                ("AutoScalingGroupName", group),
                ("MinSize", "0"),
                ("MaxSize", "3"),
                ("DesiredCapacity", "0"),
                ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
                ("AvailabilityZones.member.1", &zone),
            ],
        )
        .await;

        let mut put: Vec<(&str, &str)> = vec![
            ("AutoScalingGroupName", group),
            ("ScheduledActionName", name),
        ];
        put.extend_from_slice(definition);
        form_call(
            &router,
            case,
            region,
            "autoscaling",
            "2011-01-01",
            "PutScheduledUpdateGroupAction",
            &put,
        )
        .await;

        let answer = form_call(
            &router,
            case,
            region,
            "autoscaling",
            "2011-01-01",
            "DescribeScheduledActions",
            &[("AutoScalingGroupName", group)],
        )
        .await;
        let action = action_named(&answer, name).unwrap_or_else(|| {
            panic!("case {case:?}: the read reports no action {name:?}: {answer}")
        });
        // WHEN it next runs: the instant the put stated, or the next occurrence of the
        // recurrence it stated instead.
        let start_time = opt_element(action, "StartTime").unwrap_or_default();
        assert!(
            start_time.ends_with(runs_at),
            "case {case:?}: expected the action to next run at {runs_at:?}, got \
             {start_time:?} in {action}"
        );
        // The ARN names this Region, this group and this action.
        let arn = opt_element(action, "ScheduledActionARN").unwrap_or_default();
        let head = format!("arn:aws:autoscaling:{region}:");
        let tail = format!(":autoScalingGroupName/{group}:scheduledActionName/{name}");
        assert!(
            arn.starts_with(&head) && arn.ends_with(&tail),
            "case {case:?}: expected an ARN opening {head:?} and ending {tail:?}, got {arn:?}"
        );

        // A scheduled action only exists against a group that does.
        form_call(
            &router,
            case,
            region,
            "autoscaling",
            "2011-01-01",
            "DeleteAutoScalingGroup",
            &[("AutoScalingGroupName", group)],
        )
        .await;
        let after = form_call(
            &router,
            case,
            region,
            "autoscaling",
            "2011-01-01",
            "DescribeScheduledActions",
            &[],
        )
        .await;
        assert!(
            reported_actions(&after).is_empty(),
            "case {case:?}: expected the deleted group's action to go with it, got {after}"
        );
    }
}
