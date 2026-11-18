// SPDX-License-Identifier: CC0-1.0

//! An Elastic Beanstalk environment's lifecycle events reach the queue subscribed to the
//! Amazon SNS topic its configuration attaches — through the WHOLE mounted service set, the
//! one `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test: **an Elastic Beanstalk environment whose resolved configuration
//! names an Amazon SNS topic in the `aws:elasticbeanstalk:sns:topics` namespace has every
//! event this service records about it published to THAT topic and to no other, whichever
//! path recorded the event — the lifecycle call itself, or the clock completing the work
//! that call started — so a queue subscribed to the attached topic (and granting the SNS
//! service principal the send) receives one message per event, carrying the attached
//! `TopicArn` and naming the environment, while a topic the configuration does not name
//! receives nothing.**
//!
//! Reaching that notification at all means the two lifecycle state machines in front of it
//! have to settle, which is the other half of what this suite pins: a
//! `CreateApplicationVersion(Process=true)` leaves `PROCESSING` for `PROCESSED`, a
//! `CreateEnvironment` settles at `Ready`, `DescribeConfigurationSettings` reads the
//! attached topic back off the environment, and an `UpdateEnvironment` answers `Updating`
//! and settles back to `Ready`. A client that polls for any of those and never sees it
//! waits for ever, and then no notification is owed at all.
//!
//! Five services have to agree for that to hold — Elastic Beanstalk records and publishes,
//! Amazon S3 holds the source bundle its pre-processing downloads, SNS fans the publish out,
//! SQS holds what arrived — and the publish Elastic Beanstalk makes is an in-process call
//! addressed the way a client's own `sns`-scoped call is addressed, so WHICH mounted model
//! claims it is only decidable with every service mounted. The owning crate's suite
//! (`services/service-com-amazonaws-elasticbeanstalk-awselasticbeanstalkservice/tests/all/\
//! environment_event_notifications.rs`) pins HOW the notification target is configured —
//! by topic ARN, by endpoint, by endpoint and topic name, by nothing — over its own table;
//! what only this suite can reach is the same path over the production service set, with
//! the version-processing and environment-lifecycle legs in front of it.
//!
//! The rows differ along what the rule generalises over: the Region the whole wiring lives
//! in, how the subscribed queue's policy scopes the grant, and WHAT the update changes (one
//! configuration option setting, or the application version the environment runs) — because
//! the rule is about the events an environment records, not about which call recorded them.

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;
use smithy_server_runtime::MultiServiceRouter;

use crate::autoscaling_notification_reaches_its_subscribed_queue::{form_call, sqs};
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    authorization, given_topic_with_subscriber, router, Grant,
};
use crate::query_wire::{blocks_of, element, members_of, opt_element};

/// Elastic Beanstalk's API version, as its model declares it (`version: 2010-12-01`), and
/// SNS's (`version: 2010-03-31`).
const EB_API_VERSION: &str = "2010-12-01";
const SNS_API_VERSION: &str = "2010-03-31";

/// The configuration-option namespace and option name AWS's published Elastic Beanstalk
/// option reference puts an environment's notification topic under
/// (<https://docs.aws.amazon.com/elasticbeanstalk/latest/dg/using-features.managing.sns.html>).
/// Spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make this suite pass.
const SNS_TOPICS_NAMESPACE: &str = "aws:elasticbeanstalk:sns:topics";
const NOTIFICATION_TOPIC_ARN: &str = "Notification Topic ARN";

/// The two `ApplicationVersionStatus` values a `Process=true` create moves through, as the
/// service puts them on the wire, and the two `EnvironmentStatus` values an update moves
/// through. Spelled out for the same reason.
const VERSION_PROCESSING: &str = "PROCESSING";
const VERSION_PROCESSED: &str = "PROCESSED";
const ENVIRONMENT_READY: &str = "Ready";
const ENVIRONMENT_UPDATING: &str = "Updating";

/// The application, the environment and the two application versions every row builds.
const APPLICATION: &str = "router-notified-app";
const ENVIRONMENT: &str = "router-notified-env";
const FIRST_VERSION: &str = "v1";
const SECOND_VERSION: &str = "v2";

/// The key each version's source bundle is uploaded under, inside the account's own Elastic
/// Beanstalk storage location.
const FIRST_BUNDLE_KEY: &str = "router-notified-app/v1.zip";
const SECOND_BUNDLE_KEY: &str = "router-notified-app/v2.zip";

/// A source bundle carrying no entries: ZIP's end-of-central-directory record alone — the
/// `PK\x05\x06` signature followed by the eighteen zero bytes an empty central directory
/// occupies.
///
/// A bundle with no `env.yaml` and no `.ebextensions/*.config` has none of the files
/// `CreateApplicationVersion$Process` validates, so it has nothing to fail on and
/// pre-processing passes it — which is all this suite needs from the archive, whose subject
/// is the notification rather than the bundle. WHICH archive forms pre-processing accepts
/// and rejects is the owning crate's own table
/// (`tests/all/application_version_processing.rs`), which builds its archives with the
/// maintained `zip` crate; `core/router/Cargo.toml` is a pinned path in this repository, so
/// no dev-dependency can be added to this test crate at all.
/// The record is 22 bytes: the 4-byte signature, then the two disk numbers, the two entry
/// counts, the directory's size and offset and the comment length — 18 bytes, all zero for
/// an archive holding nothing.
const EMPTY_SOURCE_BUNDLE: &[u8] = &[
    0x50, 0x4b, 0x05, 0x06, // the end-of-central-directory signature
    0, 0, // the number of this disk
    0, 0, // the disk the central directory starts on
    0, 0, // the directory entries on this disk
    0, 0, // the directory entries in total
    0, 0, 0, 0, // the directory's size
    0, 0, 0, 0, // the directory's offset
    0, 0, // the length of the archive comment
];

/// How long a leg waits for a clock-owed transition or a delivery before calling it
/// undelivered, and how long it waits between looks.
///
/// Every wait here is for a CONDITION — the status the environment reports, the envelope the
/// queue holds — so the deadline is a backstop against hanging rather than a measurement:
/// it is many of the service's own transition spans (`eb::state::STATUS_TRANSITION_SPAN`,
/// three seconds), and the only thing that budget costs is how long a genuinely broken
/// emulator takes to fail. Nothing here asserts on elapsed time.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// One Elastic Beanstalk call over its awsQuery wire, which must answer 200.
async fn elasticbeanstalk(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    form_call(
        router,
        case,
        region,
        "elasticbeanstalk",
        EB_API_VERSION,
        action,
        params,
    )
    .await
}

/// One SNS call over its awsQuery wire, which must answer 200.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    form_call(router, case, region, "sns", SNS_API_VERSION, action, params).await
}

/// Store the source bundle `bytes` at `key` inside `bucket`, over the Amazon S3 front door,
/// so the archive Elastic Beanstalk's pre-processing downloads is one object storage really
/// holds. The bucket is the account's own storage location, which
/// `CreateStorageLocation` has already stood up.
async fn put_bundle(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    bucket: &str,
    key: &str,
    bytes: &[u8],
) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{bucket}/{key}"))
        .header("host", format!("s3.{region}.amazonaws.com"))
        .header("authorization", authorization("s3", region))
        .body(Bytes::copy_from_slice(bytes))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert_eq!(
        status,
        200,
        "case {case:?}: expected HTTP 200 from PutObject of {key}, got {status} with {}",
        String::from_utf8_lossy(response.body())
    );
}

/// Every SNS envelope waiting on the queue at `url`, deleting what it hands over so a later
/// look reads only what arrived since.
///
/// The envelope is handed back whole, rather than with its `Message` parsed: an Elastic
/// Beanstalk notification's body is the service's own text report of the event, not a
/// document, and what the rule is about is the envelope's `TopicArn` beside it.
async fn drain(router: &MultiServiceRouter, case: &str, region: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        region,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the body is not the SNS envelope ({error}): {body}")
        }));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                region,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
    }
    envelopes
}

/// Whether one envelope's `Message` or `Subject` carries `text`.
fn carries(envelope: &Value, text: &str) -> bool {
    ["Message", "Subject"].iter().any(|member| {
        envelope[*member]
            .as_str()
            .is_some_and(|value| value.contains(text))
    })
}

/// Read the queue at `url` until an envelope carrying `wanted` has arrived or the deadline
/// passes, and answer every envelope that arrived while waiting.
///
/// A describe is made between looks: the completion of a lifecycle operation is owed by the
/// CLOCK, and it is the next call that observes it as due which materialises it, records its
/// event and publishes what that event owes — exactly as a client polling for `Ready` drives
/// it. The wait is for the CONDITION, never for a fixed time.
async fn await_notification(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    url: &str,
    wanted: &str,
) -> Vec<Value> {
    let mut arrived: Vec<Value> = Vec::new();
    let deadline = tokio::time::Instant::now() + POLL_DEADLINE;
    loop {
        arrived.extend(drain(router, case, region, url).await);
        if arrived.iter().any(|envelope| carries(envelope, wanted)) {
            return arrived;
        }
        if tokio::time::Instant::now() >= deadline {
            return arrived;
        }
        described_environment(router, case, region).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What `DescribeEnvironments` reports for this suite's environment right now.
async fn described_environment(router: &MultiServiceRouter, case: &str, region: &str) -> String {
    elasticbeanstalk(
        router,
        case,
        region,
        "DescribeEnvironments",
        &[
            ("ApplicationName", APPLICATION),
            ("EnvironmentNames.member.1", ENVIRONMENT),
        ],
    )
    .await
}

/// The environment's `Status` as `DescribeEnvironments` reports it right now.
async fn environment_status(router: &MultiServiceRouter, case: &str, region: &str) -> String {
    let described = described_environment(router, case, region).await;
    opt_element(&described, "Status").unwrap_or_else(|| {
        panic!("case {case:?}: DescribeEnvironments reports no Status: {described}")
    })
}

/// Wait until the environment reports `wanted`, or the deadline passes; answer what it
/// reported last.
async fn await_environment_status(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    wanted: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + POLL_DEADLINE;
    loop {
        let status = environment_status(router, case, region).await;
        if status == wanted || tokio::time::Instant::now() >= deadline {
            return status;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `Status` `DescribeApplicationVersions` reports for `label` right now.
async fn version_status(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    label: &str,
) -> String {
    let described = elasticbeanstalk(
        router,
        case,
        region,
        "DescribeApplicationVersions",
        &[
            ("ApplicationName", APPLICATION),
            ("VersionLabels.member.1", label),
        ],
    )
    .await;
    opt_element(&described, "Status").unwrap_or_else(|| {
        panic!("case {case:?}: DescribeApplicationVersions reports no Status: {described}")
    })
}

/// Wait until `label` reports `wanted`, or the deadline passes; answer what it reported last.
async fn await_version_status(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    label: &str,
    wanted: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + POLL_DEADLINE;
    loop {
        let status = version_status(router, case, region, label).await;
        if status == wanted || tokio::time::Instant::now() >= deadline {
            return status;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The values `DescribeConfigurationSettings` reports for one namespace/option pair, one per
/// configuration set it answers with.
///
/// Each set's own `OptionSettings` list is read on its own, and each entry's `Value` is read
/// off the `<member>` that carries BOTH the namespace and the option name — so an answer
/// that reported the pair twice, or not at all, is visible here instead of being papered
/// over by a document-wide read.
fn reported_option_values(body: &str, namespace: &str, option: &str) -> Vec<Vec<String>> {
    blocks_of(body, "OptionSettings")
        .into_iter()
        .map(|settings| {
            members_of(settings)
                .into_iter()
                .filter(|member| {
                    opt_element(member, "Namespace").as_deref() == Some(namespace)
                        && opt_element(member, "OptionName").as_deref() == Some(option)
                })
                .map(|member| opt_element(member, "Value").unwrap_or_default())
                .collect()
        })
        .collect()
}

/// The platform every environment here runs on: the current version of the first branch of
/// the service's OWN published catalog (`eb::state::SOLUTION_STACKS`, what
/// `ListAvailableSolutionStacks` reports), named by its solution stack name.
fn solution_stack() -> String {
    eb::state::SOLUTION_STACKS
        .first()
        .expect("the service publishes at least one solution stack")
        .name()
}

/// One way of driving an environment update, and the wiring it is driven over.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The Region the whole wiring lives in.
    region: &'static str,
    /// How the subscribed queue's policy grants SNS the send.
    grant: Grant,
    /// The configuration option setting the update changes, or `None` for an update that
    /// deploys the second application version instead.
    changed_option: Option<(&'static str, &'static str, &'static str)>,
}

const ROWS: &[Row] = &[
    Row {
        // `aws:autoscaling:asg` / `MinSize` — "the minimum number of instances you want in
        // your Auto Scaling group", one of the options the service's own catalog publishes
        // (`eb::state::GENERAL_CONFIGURATION_OPTIONS`), whose published default is 1.
        case: "an update that changes one configuration option setting",
        region: "us-west-2",
        grant: Grant::ScopedToTopic,
        changed_option: Some(("aws:autoscaling:asg", "MinSize", "2")),
    },
    Row {
        case: "an update that deploys the other application version, in another Region",
        region: "eu-west-1",
        grant: Grant::Unscoped,
        changed_option: None,
    },
];

#[tokio::test]
async fn an_environments_events_reach_the_queue_subscribed_to_its_attached_topic() {
    for row in ROWS {
        let Row {
            case,
            region,
            grant,
            changed_option,
        } = *row;
        let router = router();

        // The topic the environment's configuration ATTACHES, and a second topic of the
        // same account that it does not: the control. Both are live SNS topics with a queue
        // of this emulator subscribed, so a control queue that stays empty says the
        // environment published nowhere else — not that the wiring is dead.
        let (topic, queue) =
            given_topic_with_subscriber(&router, case, region, "eb-environment-events", grant)
                .await;
        let (control_topic, control_queue) =
            given_topic_with_subscriber(&router, case, region, "eb-control-events", grant).await;

        // ----- the application and the two pre-processed application versions -----
        elasticbeanstalk(
            &router,
            case,
            region,
            "CreateApplication",
            &[("ApplicationName", APPLICATION)],
        )
        .await;
        let bucket = element(
            &elasticbeanstalk(&router, case, region, "CreateStorageLocation", &[]).await,
            "S3Bucket",
        );
        for key in [FIRST_BUNDLE_KEY, SECOND_BUNDLE_KEY] {
            put_bundle(
                &router,
                case,
                region,
                &bucket,
                key,
                EMPTY_SOURCE_BUNDLE,
            )
            .await;
        }
        for (label, key) in [
            (FIRST_VERSION, FIRST_BUNDLE_KEY),
            (SECOND_VERSION, SECOND_BUNDLE_KEY),
        ] {
            let created = elasticbeanstalk(
                &router,
                case,
                region,
                "CreateApplicationVersion",
                &[
                    ("ApplicationName", APPLICATION),
                    ("VersionLabel", label),
                    ("SourceBundle.S3Bucket", &bucket),
                    ("SourceBundle.S3Key", key),
                    ("Process", "true"),
                ],
            )
            .await;
            // The work the create started is not done when the call returns, so it answers
            // the transient status.
            assert_eq!(
                opt_element(&created, "Status").as_deref(),
                Some(VERSION_PROCESSING),
                "case {case:?}: {label} must be created {VERSION_PROCESSING}, got {created}"
            );
        }
        for label in [FIRST_VERSION, SECOND_VERSION] {
            let settled = await_version_status(&router, case, region, label, VERSION_PROCESSED).await;
            assert_eq!(
                settled, VERSION_PROCESSED,
                "case {case:?}: {label} must leave {VERSION_PROCESSING} for \
                 {VERSION_PROCESSED}, got {settled:?}"
            );
        }

        // ----- the environment, with the topic attached at create time -----
        let stack = solution_stack();
        elasticbeanstalk(
            &router,
            case,
            region,
            "CreateEnvironment",
            &[
                ("ApplicationName", APPLICATION),
                ("EnvironmentName", ENVIRONMENT),
                ("SolutionStackName", &stack),
                ("VersionLabel", FIRST_VERSION),
                ("OptionSettings.member.1.Namespace", SNS_TOPICS_NAMESPACE),
                ("OptionSettings.member.1.OptionName", NOTIFICATION_TOPIC_ARN),
                ("OptionSettings.member.1.Value", &topic),
            ],
        )
        .await;
        let launched = await_environment_status(&router, case, region, ENVIRONMENT_READY).await;
        assert_eq!(
            launched, ENVIRONMENT_READY,
            "case {case:?}: the launched environment must settle at {ENVIRONMENT_READY}, got \
             {launched:?}"
        );

        // The attached topic is part of what the environment IS, so its own configuration
        // reads it back — exactly once per configuration set, under the namespace and option
        // AWS's option reference names.
        let settings = elasticbeanstalk(
            &router,
            case,
            region,
            "DescribeConfigurationSettings",
            &[
                ("ApplicationName", APPLICATION),
                ("EnvironmentName", ENVIRONMENT),
            ],
        )
        .await;
        let reported = reported_option_values(&settings, SNS_TOPICS_NAMESPACE, NOTIFICATION_TOPIC_ARN);
        assert!(
            !reported.is_empty(),
            "case {case:?}: DescribeConfigurationSettings must answer a configuration set, got \
             {settings}"
        );
        for values in &reported {
            assert_eq!(
                values.as_slice(),
                [topic.clone()].as_slice(),
                "case {case:?}: each configuration set must carry exactly one \
                 {NOTIFICATION_TOPIC_ARN:?} entry, valued {topic}, got {values:?} in {settings}"
            );
        }

        // The launch's own events are already owed to the attached topic: taking them off
        // the queue here is what makes the rest of this row about the UPDATE's events.
        let launch_notifications = await_notification(
            &router,
            case,
            region,
            &queue,
            &format!("Successfully launched environment: {ENVIRONMENT}"),
        )
        .await;
        assert!(
            launch_notifications
                .iter()
                .any(|envelope| carries(envelope, ENVIRONMENT)),
            "case {case:?}: the launch's events must reach the attached topic's queue, got \
             {launch_notifications:?}"
        );
        for envelope in &launch_notifications {
            assert_eq!(
                envelope["TopicArn"].as_str(),
                Some(topic.as_str()),
                "case {case:?}: every notification must carry the attached TopicArn, got \
                 {envelope}"
            );
        }

        // ----- the update -----
        let mut params = vec![
            ("ApplicationName", APPLICATION),
            ("EnvironmentName", ENVIRONMENT),
        ];
        match changed_option {
            Some((namespace, option, value)) => {
                params.push(("OptionSettings.member.1.Namespace", namespace));
                params.push(("OptionSettings.member.1.OptionName", option));
                params.push(("OptionSettings.member.1.Value", value));
            }
            None => params.push(("VersionLabel", SECOND_VERSION)),
        }
        let updated = elasticbeanstalk(&router, case, region, "UpdateEnvironment", &params).await;
        assert_eq!(
            opt_element(&updated, "EnvironmentName").as_deref(),
            Some(ENVIRONMENT),
            "case {case:?}: UpdateEnvironment must answer about the environment, got {updated}"
        );
        assert_eq!(
            opt_element(&updated, "Status").as_deref(),
            Some(ENVIRONMENT_UPDATING),
            "case {case:?}: UpdateEnvironment must answer {ENVIRONMENT_UPDATING}, got {updated}"
        );
        let settled = await_environment_status(&router, case, region, ENVIRONMENT_READY).await;
        assert_eq!(
            settled, ENVIRONMENT_READY,
            "case {case:?}: the update must settle back to {ENVIRONMENT_READY}, got {settled:?}"
        );

        // The update's completion is an event about this environment, so it is published to
        // the attached topic and the subscribed queue receives it.
        let completion = "updateEnvironment completed successfully.";
        let delivered = await_notification(&router, case, region, &queue, completion).await;
        let notification = delivered
            .iter()
            .find(|envelope| carries(envelope, completion))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a notification carrying {completion:?} on {topic}, \
                     got {delivered:?}"
                )
            });
        assert_eq!(
            notification["TopicArn"].as_str(),
            Some(topic.as_str()),
            "case {case:?}: the notification must carry the attached TopicArn, got {notification}"
        );
        assert!(
            carries(notification, ENVIRONMENT),
            "case {case:?}: the notification must name the environment in its Subject or \
             Message, got {notification}"
        );
        assert!(
            carries(notification, APPLICATION),
            "case {case:?}: the notification must name the application the environment runs, \
             got {notification}"
        );

        // ----- and nowhere else -----
        let control = drain(&router, case, region, &control_queue).await;
        assert!(
            control.is_empty(),
            "case {case:?}: {control_topic} is attached to no environment and must receive \
             none of its events, got {control:?}"
        );
        // The calibration: that control queue is empty because nothing was published to its
        // topic, not because its wiring is dead. A direct publish reaches it.
        let marker = "a direct publish to the unattached topic";
        sns(
            &router,
            case,
            region,
            "Publish",
            &[("TopicArn", &control_topic), ("Message", marker)],
        )
        .await;
        let calibration = await_notification(&router, case, region, &control_queue, marker).await;
        assert!(
            calibration
                .iter()
                .any(|envelope| carries(envelope, marker)),
            "case {case:?}: a direct publish to {control_topic} must reach its queue, got \
             {calibration:?}"
        );
    }
}
