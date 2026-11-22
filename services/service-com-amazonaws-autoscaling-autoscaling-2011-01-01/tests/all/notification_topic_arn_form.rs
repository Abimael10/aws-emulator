// SPDX-License-Identifier: CC0-1.0

//! The form a notification configuration's `TopicARN` has to take, over the awsQuery wire.
//!
//! The RULE under test: `PutNotificationConfiguration$TopicARN` and
//! `DeleteNotificationConfiguration$TopicARN` are documented as "The Amazon Resource Name
//! (ARN) of the Amazon SNS topic"
//! (`smithy_query(query="doc", shape="PutNotificationConfigurationType", member="TopicARN")`),
//! and the member is modelled as free text bounded by length alone — so the service holds
//! the value to that form itself: it has to be a well-formed resource ARN of the Amazon SNS
//! namespace naming a topic in a Region. Anything else names no topic the group could ever
//! be notified through, so the call is refused `ValidationError` (HTTP 400) with
//! "Invalid TopicARN: " followed by the value the request carried, and the group's stored
//! configurations do not change.
//!
//! The rows span what the rule generalises over: HOW a value misses the form (no ARN
//! separators at all, a bare topic name, an endpoint URL, a truncated ARN, an ARN naming no
//! topic, an ARN of another service's namespace, the namespace in another case, an ARN
//! naming no Region, a Region label outside the grammar and one character past its published
//! bound, a missing partition, a first field that is not `arn`), and, for the values the
//! rule ADMITS, what varies between two topics (the Region, the account, the partition, the
//! topic name's own form, and a Region label at the published bound).
//!
//! Every refusal row reads the group's configurations back through
//! `DescribeNotificationConfigurations`, so no row can pass on a refusal that still wrote,
//! and every admitted row reads its configuration back the same way, so none can pass on an
//! accepted-but-unstored change.

use crate::delete_notification_configuration::{
    configured, delete_configuration, put_configuration, topic_arn, TEST_NOTIFICATION,
};
use crate::lifecycle_hooks::create_group;
use crate::put_notification_configuration_event_types::put_configuration_result;
use crate::support::{new_engine, router_with, REGION};

/// One event type of the catalog `DescribeAutoScalingNotificationTypes` publishes, spelled
/// out here rather than imported so a wrong constant in the implementation cannot make a
/// row pass. Every row configures this one type: this suite is about the TOPIC, not about
/// the events it is told about.
const INSTANCE_LAUNCH: &str = "autoscaling:EC2_INSTANCE_LAUNCH";

/// The longest Region label an AWS ARN pattern admits, read from the workspace's one
/// statement of that bound (`service_common::arn::MAX_REGION_LENGTH`, which states it as
/// every model's `*Arn` pattern does, `[a-z0-9-]{1,20}`) rather than written out here.
const MAX_REGION_LENGTH: usize = ::service_common::arn::MAX_REGION_LENGTH;

/// A topic ARN with each field named: `arn:<partition>:sns:<region>:<account>:<topic>`.
fn topic(partition: &str, region: &str, account: &str, name: &str) -> String {
    format!("arn:{partition}:sns:{region}:{account}:{name}")
}

/// The account every resource this emulator holds belongs to.
fn account() -> String {
    ::engine::emulator_account_id().to_string()
}

/// The whole message a request whose `TopicARN` names no Amazon SNS topic is refused with,
/// written out here rather than taken from the service's own builder: the fixed text, and
/// the value the request carried as its only variable part.
fn invalid_topic_arn_message(value: &str) -> String {
    format!("Invalid TopicARN: {value}")
}

/// The whole message a delete naming a topic the group holds no configuration for is
/// refused with, written out here for the same reason: Amazon EC2 Auto Scaling reports the
/// absence as the topic not existing for the account.
fn absent_configuration_message(value: &str) -> String {
    format!("Notification Topic '{value}' doesn't exist for '{}'", account())
}

/// `(case, the value)` for every row that misses the form, each differing in HOW it misses
/// it.
fn malformed_rows() -> Vec<(String, String)> {
    let account = account();
    let too_long_region = "a".repeat(MAX_REGION_LENGTH + 1);
    vec![
        (
            "text carrying no ARN separators at all".to_string(),
            "not-an-arn".to_string(),
        ),
        (
            "a bare topic name".to_string(),
            "notification-topic".to_string(),
        ),
        (
            "the topic's HTTPS endpoint rather than its ARN".to_string(),
            format!("https://sns.{REGION}.amazonaws.com/{account}/notification-topic"),
        ),
        (
            "an ARN truncated before its resource field".to_string(),
            format!("arn:aws:sns:{REGION}:{account}"),
        ),
        (
            "an ARN naming no topic".to_string(),
            format!("arn:aws:sns:{REGION}:{account}:"),
        ),
        (
            "an ARN of another service's namespace".to_string(),
            format!("arn:aws:sqs:{REGION}:{account}:notification-queue"),
        ),
        (
            "an ARN of this service's own namespace".to_string(),
            format!("arn:aws:autoscaling:{REGION}:{account}:autoScalingGroup:g:autoScalingGroupName/g"),
        ),
        (
            "the Amazon SNS namespace in another case".to_string(),
            format!("arn:aws:SNS:{REGION}:{account}:notification-topic"),
        ),
        (
            "an ARN naming no Region".to_string(),
            topic("aws", "", &account, "notification-topic"),
        ),
        (
            "a Region label outside the grammar".to_string(),
            topic("aws", "US-WEST-2", &account, "notification-topic"),
        ),
        (
            format!("a Region label one character past the {MAX_REGION_LENGTH}-character bound"),
            topic("aws", &too_long_region, &account, "notification-topic"),
        ),
        (
            "an ARN carrying no partition".to_string(),
            format!("arn::sns:{REGION}:{account}:notification-topic"),
        ),
        (
            "a value whose first field is not `arn`".to_string(),
            format!("aws:sns:{REGION}:{account}:notification-topic"),
        ),
    ]
}

/// `(case, the value)` for every row the rule ADMITS, each differing in what varies between
/// two real topics.
fn admitted_rows() -> Vec<(String, String)> {
    let account = account();
    let at_the_bound = "a".repeat(MAX_REGION_LENGTH);
    vec![
        (
            "a topic of this account in the Region the request is signed for".to_string(),
            topic("aws", REGION, &account, "notification-topic"),
        ),
        (
            "a topic in another Region".to_string(),
            topic("aws", "eu-central-1", &account, "notification-topic"),
        ),
        (
            "a topic of another account".to_string(),
            topic("aws", REGION, "210987654321", "cross-account-topic"),
        ),
        (
            "a FIFO topic, whose name carries the suffix".to_string(),
            topic("aws", REGION, &account, "notification-topic.fifo"),
        ),
        (
            "a topic in another partition".to_string(),
            topic("aws-us-gov", "us-gov-west-1", &account, "gov-topic"),
        ),
        (
            format!("a Region label at the {MAX_REGION_LENGTH}-character bound"),
            topic("aws", &at_the_bound, &account, "notification-topic"),
        ),
    ]
}

/// Every `TopicARN` outside the form is refused on the input, and nothing is stored for it.
#[tokio::test]
async fn a_topic_arn_outside_the_form_is_refused() {
    let router = router_with(new_engine());
    let group = "topic-form-group";
    create_group(&router, group).await;

    for (case, value) in malformed_rows() {
        let (status, code, message) =
            put_configuration_result(&router, group, &value, &[INSTANCE_LAUNCH.to_string()]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: PutNotificationConfiguration({value:?}) expected 400, got {status} \
             with code {code:?} and message {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected error code \"ValidationError\", got {code:?} with message \
             {message:?}"
        );
        assert_eq!(
            message,
            invalid_topic_arn_message(&value),
            "case {case:?}: expected the refusal to name the member and the value, got \
             {message:?}"
        );
        let stored = configured(&router, &case, group).await;
        assert!(
            stored.is_empty(),
            "case {case:?}: a refused configuration stores nothing, got {stored:?}"
        );
    }
}

/// A refusal leaves the configurations the group already holds exactly as they were.
#[tokio::test]
async fn a_refused_topic_arn_leaves_the_stored_configurations_alone() {
    let router = router_with(new_engine());
    let group = "topic-form-collateral-group";
    create_group(&router, group).await;

    let case = "the good configuration stored first";
    let stored_topic = topic_arn("already-configured-topic");
    put_configuration(&router, case, group, &stored_topic, &[TEST_NOTIFICATION]).await;
    let expected = vec![(stored_topic.clone(), TEST_NOTIFICATION.to_string())];

    for (case, value) in malformed_rows() {
        let (status, _, message) =
            put_configuration_result(&router, group, &value, &[INSTANCE_LAUNCH.to_string()]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: PutNotificationConfiguration({value:?}) expected 400, got {status} \
             with message {message:?}"
        );
        let stored = configured(&router, &case, group).await;
        assert_eq!(
            stored, expected,
            "case {case:?}: the refusal leaves the one stored configuration in place, got \
             {stored:?}"
        );
    }
}

/// Every `TopicARN` the rule admits is stored, and reads back on the group.
#[tokio::test]
async fn a_topic_arn_inside_the_form_is_stored() {
    for (row, (case, value)) in admitted_rows().into_iter().enumerate() {
        let router = router_with(new_engine());
        let group = format!("admitted-topic-group-{row}");
        create_group(&router, &group).await;

        let (status, code, message) =
            put_configuration_result(&router, &group, &value, &[INSTANCE_LAUNCH.to_string()]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutNotificationConfiguration({value:?}) expected 200, got {status} \
             with code {code:?} and message {message:?}"
        );
        let stored = configured(&router, &case, &group).await;
        assert_eq!(
            stored,
            vec![(value.clone(), INSTANCE_LAUNCH.to_string())],
            "case {case:?}: the stored configuration reads back on the group, got {stored:?}"
        );
    }
}

/// The delete reads its `TopicARN` against the same form: a value outside it is refused as
/// a bad request, while a well-formed topic the group was never configured with is refused
/// as the absent configuration it is.
#[tokio::test]
async fn the_delete_reads_its_topic_arn_against_the_same_form() {
    let router = router_with(new_engine());
    let group = "topic-form-delete-group";
    create_group(&router, group).await;

    let case = "the configuration the delete must not disturb";
    let stored_topic = topic_arn("delete-witness-topic");
    put_configuration(&router, case, group, &stored_topic, &[TEST_NOTIFICATION]).await;
    let expected = vec![(stored_topic.clone(), TEST_NOTIFICATION.to_string())];

    for (case, value) in malformed_rows() {
        let (status, code, message) = delete_configuration(&router, group, &value).await;
        assert_eq!(
            status, 400,
            "case {case:?}: DeleteNotificationConfiguration({value:?}) expected 400, got \
             {status} with code {code:?} and message {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected error code \"ValidationError\", got {code:?} with message \
             {message:?}"
        );
        assert_eq!(
            message,
            invalid_topic_arn_message(&value),
            "case {case:?}: expected the refusal to name the member and the value, got \
             {message:?}"
        );
        let stored = configured(&router, &case, group).await;
        assert_eq!(
            stored, expected,
            "case {case:?}: the refused delete leaves the stored configuration in place, got \
             {stored:?}"
        );
    }

    // A value INSIDE the form naming a topic this group holds no configuration for is a
    // different refusal: there is nothing to delete, not a value that is no topic.
    let never_configured = topic_arn("never-configured-topic");
    let (status, code, message) = delete_configuration(&router, group, &never_configured).await;
    assert_eq!(
        status, 400,
        "a delete naming no stored configuration expected 400, got {status} with message \
         {message:?}"
    );
    assert_eq!(
        code, "ValidationError",
        "a delete naming no stored configuration expected \"ValidationError\", got {code:?}"
    );
    assert_eq!(
        message,
        absent_configuration_message(&never_configured),
        "a well-formed topic the group holds no configuration for is refused as the absent \
         configuration it is, got {message:?}"
    );
}
