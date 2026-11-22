// SPDX-License-Identifier: CC0-1.0

//! What the lifecycle-hook and notification-configuration surface of an Auto Scaling
//! group REFUSES, and what it answers instead, over the awsQuery wire.
//!
//! The rules under test, each one sentence:
//!
//! * a lifecycle hook's notification target names a resource of a service Amazon EC2
//!   Auto Scaling can publish to, so a target ARN of any other vendor is refused — on
//!   the VALUE, before the group the request names is resolved — and the refusal names
//!   the vendor the ARN carried and the vendors that are supported;
//! * a refusal of a member value outside the set or the range that member admits states
//!   the member and what it admits and carries no part of the request;
//! * the lifecycle-hook READ of a group reports the hooks registered for the name it was
//!   given, so a name no group answers to reports none rather than refusing the read;
//! * a call addressed at one hook of one group is refused when no such hook is
//!   registered, naming both identifiers it was addressed by;
//! * the absent-group refusal of a notification configuration names the group the
//!   request asked for;
//! * a notification configuration the delete took off a group is no longer reported by
//!   the read it was reported by;
//! * a paginated read refuses a token it did not issue with the refusal that read
//!   serves — its own error code and its own sentence — quoting the token the caller
//!   submitted.
//!
//! The tables span what those rules generalise over: the vendor an ARN names (each
//! supported one, several unsupported ones, a partition other than `aws`, the vendor in
//! the wrong case), which member carries the offending value, the group name's length,
//! case, character set and spelling, whether a name never existed or was created and
//! deleted, whether the hook was never stored or already deleted, how many topics and
//! event types a group is configured with, which paginated read is asked, and the form of
//! the token submitted. Every write row is read back through the implemented reader, so
//! no row can pass on an accepted-but-unstored change — or on a refusal that wrote
//! something anyway.
//!
//! `lifecycle_hooks.rs` owns the hook harness and `delete_notification_configuration.rs`
//! the notification harness; both are driven from here rather than copied.

use crate::delete_notification_configuration::{
    configured, delete_configuration, put_configuration, topic_arn, TEST_NOTIFICATION,
};
use crate::lifecycle_hooks::{
    absent_hook_message, create_group, delete_hook, describe_result, described_hook,
    described_hooks, heartbeat_range_message, outside_value_set, put_hook, RESULTS, TRANSITIONS,
};
use crate::support::{borrowed, new_engine, p, query, router_with, tag_value, tag_values};

/// The vendors a lifecycle hook's notification target may be a resource of, as the
/// refusal below states them.
///
/// The model documents the member as the ARN of a target Amazon EC2 Auto Scaling
/// publishes to — "You can specify either an Amazon SNS topic or an Amazon SQS queue"
/// (`smithy_query(query="doc",shape="PutLifecycleHookType",member="NotificationTargetARN")`)
/// — and the service's own refusal names the set it accepts. Spelled out here rather
/// than imported, so a wrong catalog in the implementation cannot make a row pass.
const SUPPORTED_VENDORS: [&str; 3] = ["sqs", "sns", "lambda"];

/// The whole message a target of an unsupported vendor is refused with: fixed text
/// around ONE variable part, the vendor field of the ARN the request submitted.
fn unsupported_vendor_message(vendor: &str) -> String {
    format!(
        "The value in 'NotificationTargetARN' is invalid. The specified vendor '{vendor}' is \
         not valid. Supported vendors: {}",
        SUPPORTED_VENDORS.join(", ")
    )
}

/// The whole message a call naming a group this service holds none of is refused with in
/// the detail form `PutNotificationConfiguration` serves: the fixed prefix and the name
/// the request asked for.
fn absent_group_named(name: &str) -> String {
    format!("AutoScalingGroup name not found - {name}")
}

/// What a paginated read answers a token it did not issue with: the wire error code and
/// the whole message, in the refusal each read serves.
///
/// Four forms, one per group of reads: the modelled `InvalidNextToken` stating the token
/// in the past tense, the same error stating it in the present tense
/// (`DescribeNotificationConfigurations`, `DescribeScheduledActions`), that present-tense
/// sentence terminated by a period (`DescribeInstanceRefreshes`), and the service-common
/// `ValidationError` naming the member and the value submitted for it.
#[derive(Debug, Clone, Copy)]
enum TokenRefusal {
    PastTense,
    PresentTense,
    TerminatedSentence,
    ValueViolation,
}

impl TokenRefusal {
    /// The wire error code this refusal travels with.
    fn code(self) -> &'static str {
        match self {
            Self::PastTense | Self::PresentTense | Self::TerminatedSentence => "InvalidNextToken",
            Self::ValueViolation => "ValidationError",
        }
    }

    /// The whole message this refusal states for `token`.
    fn message(self, token: &str) -> String {
        match self {
            Self::PastTense => format!("The token '{token}' was invalid"),
            Self::PresentTense => format!("The token '{token}' is invalid"),
            Self::TerminatedSentence => format!("The token '{token}' is invalid."),
            Self::ValueViolation => format!("NextToken: {token}"),
        }
    }
}

/// One notification target a request may carry: the case, the value, and the vendor the
/// refusal must report — `None` for a value the service accepts.
struct TargetRow {
    case: &'static str,
    target: String,
    refused_vendor: Option<&'static str>,
}

/// The targets this suite submits: one per supported vendor (including a partition other
/// than `aws`, and a resource field carrying further colons and slashes), and one per
/// unsupported vendor — among them the vendor spelled in the wrong case, since an ARN
/// namespace is a fixed lowercase token.
fn target_rows() -> Vec<TargetRow> {
    let account = ::engine::emulator_account_id();
    let region = crate::support::REGION;
    let accepted = [
        (
            "an Amazon SNS topic",
            format!("arn:aws:sns:{region}:{account}:asg-hook-topic"),
        ),
        (
            "an Amazon SQS queue",
            format!("arn:aws:sqs:{region}:{account}:asg-hook-queue"),
        ),
        (
            "a Lambda function",
            format!("arn:aws:lambda:{region}:{account}:function:asg-hook-fn"),
        ),
        (
            "a Lambda function version, so the resource field carries further colons",
            format!("arn:aws:lambda:{region}:{account}:function:asg-hook-fn:3"),
        ),
        (
            "an Amazon SNS topic of another partition",
            format!("arn:aws-us-gov:sns:us-gov-west-1:{account}:asg-hook-topic"),
        ),
        ("no target at all, which clears a stored one", String::new()),
        (
            "a value that carries no ARN fields at all, so it names no vendor",
            "asg-hook-topic".to_string(),
        ),
    ];
    let refused = [
        (
            "an Amazon S3 bucket, whose ARN names no Region and no account",
            "arn:aws:s3:::asg-hook-bucket".to_string(),
            "s3",
        ),
        (
            "an Amazon Kinesis stream",
            format!("arn:aws:kinesis:{region}:{account}:stream/asg-hook-stream"),
            "kinesis",
        ),
        (
            "an Amazon EventBridge event bus",
            format!("arn:aws:events:{region}:{account}:event-bus/default"),
            "events",
        ),
        (
            "an IAM role",
            format!("arn:aws:iam::{account}:role/asg-hook-role"),
            "iam",
        ),
        (
            "an Amazon DynamoDB table",
            format!("arn:aws:dynamodb:{region}:{account}:table/asg-hook-table"),
            "dynamodb",
        ),
        (
            "a supported vendor spelled in upper case",
            format!("arn:aws:SNS:{region}:{account}:asg-hook-topic"),
            "SNS",
        ),
        (
            "the Auto Scaling group itself",
            format!("arn:aws:autoscaling:{region}:{account}:autoScalingGroup:abc"),
            "autoscaling",
        ),
    ];

    accepted
        .into_iter()
        .map(|(case, target)| TargetRow {
            case,
            target,
            refused_vendor: None,
        })
        .chain(refused.into_iter().map(|(case, target, vendor)| TargetRow {
            case,
            target,
            refused_vendor: Some(vendor),
        }))
        .collect()
}

/// Every target row, through `PutLifecycleHook`: a supported vendor is stored and read
/// back, an unsupported one is refused naming its vendor and stores nothing.
#[tokio::test]
async fn a_hook_target_must_name_a_vendor_the_service_can_notify() {
    let router = router_with(new_engine());
    create_group(&router, "asg-target").await;

    for (index, row) in target_rows().iter().enumerate() {
        let hook = format!("hook-{index}");
        let (status, code, message) = put_hook(
            &router,
            "asg-target",
            &hook,
            &[
                p("LifecycleTransition", TRANSITIONS[0]),
                p("NotificationTargetARN", row.target.clone()),
            ],
        )
        .await;

        match row.refused_vendor {
            None => {
                assert_eq!(
                    status, 200,
                    "case {:?}: the target must be accepted, got {status} with {code:?} \
                     {message:?}",
                    row.case
                );
                let stored = described_hook(&router, "asg-target", &hook).await;
                assert_eq!(
                    stored.notification_target_arn, row.target,
                    "case {:?}: expected the stored target {:?}, got {:?}",
                    row.case, row.target, stored.notification_target_arn
                );
            }
            Some(vendor) => {
                assert_eq!(
                    status, 400,
                    "case {:?}: the target must be refused, got {status} with {code:?} \
                     {message:?}",
                    row.case
                );
                assert_eq!(
                    code, "ValidationError",
                    "case {:?}: expected the ValidationError code, got {code:?} {message:?}",
                    row.case
                );
                let expected = unsupported_vendor_message(vendor);
                assert_eq!(
                    message, expected,
                    "case {:?}: expected the message {expected:?}, got {message:?}",
                    row.case
                );
                let held = described_hooks(&router, "asg-target", &[&hook]).await;
                assert!(
                    held.is_empty(),
                    "case {:?}: a refused put stores no hook, got {held:?}",
                    row.case
                );
            }
        }
    }
}

/// The value rules are checked BEFORE the group the request names is resolved, so a
/// request carrying an unusable value against a name no group answers to is refused with
/// that value's own sentence — the one a client acts on — rather than with the
/// absent-group one.
#[tokio::test]
async fn a_value_rule_is_checked_before_the_group_is_resolved() {
    let router = router_with(new_engine());

    let rows = [
        (
            "an unsupported notification target",
            vec![
                p("LifecycleTransition", TRANSITIONS[0]),
                p("NotificationTargetARN", "arn:aws:s3:::asg-hook-bucket"),
            ],
            unsupported_vendor_message("s3"),
        ),
        (
            "a transition outside the published pair",
            vec![p("LifecycleTransition", "autoscaling:ec2_instance_launching")],
            outside_value_set("LifecycleTransition", &TRANSITIONS),
        ),
        (
            "a default result outside the published pair",
            vec![
                p("LifecycleTransition", TRANSITIONS[0]),
                p("DefaultResult", "PROCEED"),
            ],
            outside_value_set("DefaultResult", &RESULTS),
        ),
        (
            "a heartbeat timeout outside the published range",
            vec![
                p("LifecycleTransition", TRANSITIONS[0]),
                p("HeartbeatTimeout", "10"),
            ],
            heartbeat_range_message(),
        ),
    ];

    for (case, members, expected) in rows {
        let (status, code, message) = put_hook(&router, "asg-no-such-group", "hook", &members).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected the ValidationError code, got {code:?} {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
    }
}

/// The group CREATE carries hook specifications, so it is held to the same target rule:
/// a specification naming an unsupported vendor refuses the create, and no group is
/// stored.
#[tokio::test]
async fn a_hook_specification_of_a_create_is_held_to_the_target_rule() {
    let router = router_with(new_engine());

    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &borrowed(&[
            p("AutoScalingGroupName", "asg-spec-target"),
            p("LaunchTemplate.LaunchTemplateName", "lt-1"),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("DesiredCapacity", "0"),
            p("AvailabilityZones.member.1", "us-west-2a"),
            p(
                "LifecycleHookSpecificationList.member.1.LifecycleHookName",
                "spec-hook",
            ),
            p(
                "LifecycleHookSpecificationList.member.1.LifecycleTransition",
                TRANSITIONS[0],
            ),
            p(
                "LifecycleHookSpecificationList.member.1.NotificationTargetARN",
                "arn:aws:s3:::asg-hook-bucket",
            ),
        ]),
    )
    .await;
    assert_eq!(
        status, 400,
        "the create must be refused, got {status} with {body:?}"
    );
    assert_eq!(
        tag_value(&body, "Code"),
        "ValidationError",
        "expected the ValidationError code, got {body:?}"
    );
    let expected = unsupported_vendor_message("s3");
    assert_eq!(
        crate::support::xml_unescape(&tag_value(&body, "Message")),
        expected,
        "expected the message {expected:?}, got {body:?}"
    );

    let (status, body) = query(
        &router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", "asg-spec-target")],
    )
    .await;
    assert_eq!(status, 200, "the read-back must succeed, got {body:?}");
    let reported = tag_values(&body, "AutoScalingGroupName");
    assert!(
        reported.is_empty(),
        "the refused create stored no group, got {reported:?}"
    );
}

/// Group names no group answers to: they differ in length, case, character set and
/// spelling, so no row can pass on a name-shaped special case.
fn absent_names() -> Vec<(&'static str, String)> {
    let mut names: Vec<(&'static str, String)> = [
        ("the shortest name the model allows", "a"),
        ("a name of the shape a client generates", "asg-0123456789ab"),
        ("an upper-case name", "ASG-UPPER-CASE"),
        ("a name spelled like the unfilled detail token", "null"),
        ("a name spelled like the error code", "ValidationError"),
        ("a name that is only digits", "1234567890123456"),
        ("a non-ASCII name", "グループ-ünïcode"),
        ("a name carrying XML metacharacters", "<asg & 'name'>"),
    ]
    .into_iter()
    .map(|(case, name)| (case, name.to_string()))
    .collect();
    // The longest name `AutoScalingGroupName` admits (`XmlStringMaxLen255`, len 1..255).
    names.push(("the longest name the model allows", "z".repeat(255)));
    names
}

/// The hook READ of a name no group answers to reports no hooks, whether the name never
/// existed or named a group that was deleted — and a group that holds hooks still
/// reports them, so the read did not simply stop answering.
#[tokio::test]
async fn a_hook_read_of_an_absent_group_reports_no_hooks() {
    let router = router_with(new_engine());
    create_group(&router, "asg-held").await;
    let (status, code, message) = put_hook(
        &router,
        "asg-held",
        "hook",
        &[p("LifecycleTransition", TRANSITIONS[0])],
    )
    .await;
    assert_eq!(
        status, 200,
        "the hook the read must still report has to be created, got {status} with {code:?} \
         {message:?}"
    );

    for (case, name) in absent_names() {
        // With no filter, and with filters naming a held and an unheld hook: the answer
        // is the same empty list, because the name holds nothing to narrow.
        for filter in [vec![], vec!["hook"], vec!["hook", "hook-not-held"]] {
            let (status, code, message) = describe_result(&router, &name, &filter).await;
            assert_eq!(
                status, 200,
                "case {case:?} filtering {filter:?}: the read must be served, got {status} \
                 with {code:?} {message:?}"
            );
            let hooks = described_hooks(&router, &name, &filter).await;
            assert!(
                hooks.is_empty(),
                "case {case:?} filtering {filter:?}: expected no hooks, got {hooks:?}"
            );
        }
    }

    let held = described_hooks(&router, "asg-held", &[]).await;
    assert_eq!(
        held.len(),
        1,
        "the group that does hold a hook still reports it, got {held:?}"
    );

    // The same read of a name that WAS a group: once the group is gone its hooks are too,
    // and the read reports none instead of refusing a teardown probe.
    let (status, body) = query(
        &router,
        "DeleteAutoScalingGroup",
        &[("AutoScalingGroupName", "asg-held")],
    )
    .await;
    assert_eq!(status, 200, "the delete must succeed, got {body:?}");
    let (status, code, message) = describe_result(&router, "asg-held", &[]).await;
    assert_eq!(
        status, 200,
        "the read of the deleted group's name must be served, got {status} with {code:?} \
         {message:?}"
    );
    let hooks = described_hooks(&router, "asg-held", &[]).await;
    assert!(
        hooks.is_empty(),
        "the deleted group holds no hooks, got {hooks:?}"
    );
}

/// The DELETE of a hook that is not registered: refused, naming the hook and the group it
/// was addressed to, whether the group is absent, holds other hooks, or held this one
/// until a moment ago — while the hook that IS registered is deleted exactly once.
#[tokio::test]
async fn a_delete_of_an_unregistered_hook_names_the_hook_and_the_group() {
    let router = router_with(new_engine());
    create_group(&router, "asg-hooks").await;
    create_group(&router, "asg-other").await;
    for (group, hook) in [("asg-hooks", "hook-a"), ("asg-other", "hook-elsewhere")] {
        let (status, code, message) =
            put_hook(&router, group, hook, &[p("LifecycleTransition", TRANSITIONS[1])]).await;
        assert_eq!(
            status, 200,
            "the hook {hook:?} of {group:?} must be created, got {status} with {code:?} \
             {message:?}"
        );
    }

    // The registered hook goes on the first call and is unregistered on the second.
    let (status, code, message) = delete_hook(&router, "asg-hooks", "hook-a").await;
    assert_eq!(
        status, 200,
        "the registered hook must be deleted, got {status} with {code:?} {message:?}"
    );

    let mut rows: Vec<(String, String, String)> = vec![
        (
            "the hook this call already deleted".to_string(),
            "asg-hooks".to_string(),
            "hook-a".to_string(),
        ),
        (
            "a hook name the group never held".to_string(),
            "asg-hooks".to_string(),
            "hook-never-stored".to_string(),
        ),
        (
            "a hook of ANOTHER group, named on this one".to_string(),
            "asg-hooks".to_string(),
            "hook-elsewhere".to_string(),
        ),
        (
            "a hook name carrying the separators the member admits".to_string(),
            "asg-hooks".to_string(),
            "team_a/hook-1".to_string(),
        ),
        (
            // `LifecycleHookName` is `AsciiStringMaxLen255` (len 1..255, pattern
            // `^[A-Za-z0-9\-_\/]+$`), so this is the longest name the member admits.
            "the longest hook name the model allows".to_string(),
            "asg-hooks".to_string(),
            "h".repeat(255),
        ),
    ];
    for (case, name) in absent_names() {
        // A group the account does not hold: the refusal is still about the HOOK, and
        // names both identifiers.
        rows.push((
            format!("a group that does not exist ({case})"),
            name,
            "hook-a".to_string(),
        ));
    }

    for (case, group, hook) in rows {
        let (status, code, message) = delete_hook(&router, &group, &hook).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected the ValidationError code, got {code:?} {message:?}"
        );
        let expected = absent_hook_message(&hook, &group);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
    }

    // The refusals above took nothing off the group that still holds a hook.
    let held = described_hooks(&router, "asg-other", &[]).await;
    assert_eq!(
        held.len(),
        1,
        "the other group keeps its own hook, got {held:?}"
    );
}

/// A notification configuration for a name no group answers to: refused naming the group
/// the request asked for — by the write that configures one and by the one that takes it
/// off again — while a group the account holds is still configured.
#[tokio::test]
async fn a_notification_configuration_of_an_absent_group_names_it() {
    let router = router_with(new_engine());
    create_group(&router, "asg-notified").await;
    let topic = topic_arn("asg-notified-topic");

    for (case, name) in absent_names() {
        let (status, body) = query(
            &router,
            "PutNotificationConfiguration",
            &borrowed(&[
                p("AutoScalingGroupName", name.clone()),
                p("TopicARN", topic.clone()),
                p("NotificationTypes.member.1", TEST_NOTIFICATION),
            ]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            tag_value(&body, "Code"),
            "ValidationError",
            "case {case:?}: expected the ValidationError code, got {body:?}"
        );
        let expected = absent_group_named(&name);
        let message = crate::support::xml_unescape(&tag_value(&body, "Message"));
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );

        // The delete of a configuration is addressed at a group too, and names it the
        // same way.
        let (status, code, message) = delete_configuration(&router, &name, &topic).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the delete must be refused, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected the ValidationError code, got {code:?} {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
    }

    put_configuration(
        &router,
        "the group the account holds",
        "asg-notified",
        &topic,
        &[TEST_NOTIFICATION],
    )
    .await;
    let configured_pairs = configured(&router, "the group the account holds", "asg-notified").await;
    assert_eq!(
        configured_pairs,
        vec![(topic.clone(), TEST_NOTIFICATION.to_string())],
        "the group the account holds is configured, got {configured_pairs:?}"
    );
}

/// One deletion row: the case, the topics the group is configured with (each with its
/// event types), and which of those topics the delete names.
type DeletionRow<'a> = (&'a str, Vec<(&'a str, Vec<&'a str>)>, usize);

/// A configuration the delete took off a group is no longer reported, whatever the group
/// was configured with: the read a client verifies the removal through returns the
/// remaining entries and nothing else.
#[tokio::test]
async fn a_deleted_notification_configuration_is_no_longer_reported() {
    let router = router_with(new_engine());
    let group = "asg-unsubscribe";
    create_group(&router, group).await;

    // Rows differ in how many topics the group holds and how many event types the deleted
    // topic was configured for.
    let rows: [DeletionRow<'_>; 3] = [
        (
            "one topic configured for one event type",
            vec![("topic-1", vec![TEST_NOTIFICATION])],
            0,
        ),
        (
            "one topic configured for several event types",
            vec![(
                "topic-1",
                vec![
                    TEST_NOTIFICATION,
                    "autoscaling:EC2_INSTANCE_LAUNCH",
                    "autoscaling:EC2_INSTANCE_TERMINATE",
                ],
            )],
            0,
        ),
        (
            "the middle topic of three",
            vec![
                ("topic-1", vec![TEST_NOTIFICATION]),
                ("topic-2", vec!["autoscaling:EC2_INSTANCE_LAUNCH"]),
                ("topic-3", vec!["autoscaling:EC2_INSTANCE_TERMINATE"]),
            ],
            1,
        ),
    ];

    for (case, topics, deleted) in rows {
        for (name, events) in &topics {
            put_configuration(&router, case, group, &topic_arn(name), events).await;
        }
        let before = configured(&router, case, group).await;
        let (deleted_name, _) = topics[deleted];
        let deleted_arn = topic_arn(deleted_name);
        assert!(
            before.iter().any(|(topic, _)| *topic == deleted_arn),
            "case {case:?}: the configuration to delete must be reported first, got {before:?}"
        );

        let (status, code, message) = delete_configuration(&router, group, &deleted_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the delete must succeed, got {status} with {code:?} {message:?}"
        );

        let after = configured(&router, case, group).await;
        let expected: Vec<(String, String)> = before
            .into_iter()
            .filter(|(topic, _)| *topic != deleted_arn)
            .collect();
        assert_eq!(
            after, expected,
            "case {case:?}: expected the remaining configurations {expected:?}, got {after:?}"
        );

        // Leave the group as it was found, so each row starts from what it configures.
        for (name, _) in &topics {
            let arn = topic_arn(name);
            if arn != deleted_arn {
                let (status, code, message) = delete_configuration(&router, group, &arn).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: tearing {arn} down must succeed, got {status} with \
                     {code:?} {message:?}"
                );
            }
        }
        let left = configured(&router, case, group).await;
        assert!(
            left.is_empty(),
            "case {case:?}: every configuration was deleted, got {left:?}"
        );
    }
}

/// One paginated-read row: the action, the members it needs beside the token, and the
/// refusal it serves for a token this service did not issue.
type PaginatedRead<'a> = (&'a str, Vec<(&'a str, &'a str)>, TokenRefusal);

/// Every paginated read, for every form of token this service did not issue: HTTP 400
/// and the refusal that read serves — its own error code and its own sentence, quoting
/// the token the caller submitted.
#[tokio::test]
async fn an_unusable_next_token_is_refused_in_the_refusal_the_read_serves() {
    let router = router_with(new_engine());
    let group = "asg-paged";
    create_group(&router, group).await;

    // The paginated reads of this service, with the members each one needs beside the
    // token and the refusal it serves.
    let reads: [PaginatedRead<'_>; 9] = [
        (
            "DescribeNotificationConfigurations",
            vec![],
            TokenRefusal::PresentTense,
        ),
        (
            "DescribeAutoScalingGroups",
            vec![],
            TokenRefusal::PastTense,
        ),
        (
            "DescribeScheduledActions",
            vec![],
            TokenRefusal::PresentTense,
        ),
        (
            "DescribeAutoScalingInstances",
            vec![],
            TokenRefusal::PresentTense,
        ),
        (
            "DescribeScalingActivities",
            vec![],
            TokenRefusal::PastTense,
        ),
        (
            "DescribeWarmPool",
            vec![("AutoScalingGroupName", group)],
            TokenRefusal::PastTense,
        ),
        (
            "DescribeInstanceRefreshes",
            vec![("AutoScalingGroupName", group)],
            TokenRefusal::TerminatedSentence,
        ),
        (
            "DescribeLoadBalancerTargetGroups",
            vec![("AutoScalingGroupName", group)],
            TokenRefusal::ValueViolation,
        ),
        ("DescribeTags", vec![], TokenRefusal::ValueViolation),
    ];
    // Tokens a client can submit that this service never issued: free text, the marker
    // alone, the first page's offset (which is reached with no token at all), an offset
    // past the end of every list here, and text carrying characters the envelope escapes.
    let tokens = [
        "not-a-real-token",
        "next-",
        "next-0",
        "next-9",
        "next-abc",
        "page 2 of 3",
        "<token & 'quoted'>",
    ];

    for (action, extra, refusal) in reads {
        for token in tokens {
            let case = format!("{action} with the token {token:?}");
            let mut params: Vec<(&str, &str)> = vec![("NextToken", token)];
            params.extend_from_slice(&extra);
            let (status, body) = query(&router, action, &params).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {body:?}"
            );
            let code = tag_value(&body, "Code");
            assert_eq!(
                code,
                refusal.code(),
                "case {case:?}: expected the {:?} code, got {code:?} with {body:?}",
                refusal.code()
            );
            let expected = refusal.message(token);
            let message = crate::support::xml_unescape(&tag_value(&body, "Message"));
            assert_eq!(
                message, expected,
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );
        }
    }

    // A read that carries NO token is still served, so the refusal above is about the
    // token rather than about the read.
    let (status, body) = query(&router, "DescribeNotificationConfigurations", &[]).await;
    assert_eq!(
        status, 200,
        "a read carrying no token must be served, got {status} with {body:?}"
    );

    // A group-scoped read resolves the group it names BEFORE it reads the token, so a
    // call carrying both an unusable token and a name no group answers to is refused for
    // the group.
    for name in ["asg-no-such-group", "ASG-PAGED"] {
        let (status, body) = query(
            &router,
            "DescribeLoadBalancerTargetGroups",
            &[
                ("AutoScalingGroupName", name),
                ("NextToken", "not-a-real-token"),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400, got {status} with {body:?}"
        );
        let expected =
            format!("AutoScalingGroup name not found - Could not find Auto Scaling group \
                     named '{name}'");
        let message = crate::support::xml_unescape(&tag_value(&body, "Message"));
        assert_eq!(
            message, expected,
            "case {name:?}: expected the message {expected:?}, got {message:?}"
        );
    }
}
