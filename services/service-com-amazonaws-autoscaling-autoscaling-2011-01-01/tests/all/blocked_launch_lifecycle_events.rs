// SPDX-License-Identifier: CC0-1.0

//! What a client watching a group through EventBridge sees when a capacity change the group
//! SET OUT to make places nothing at all.
//!
//! RULE under test: a lifecycle step a group set out to take is published on the account's
//! default event bus either way — one that carried through under that step's successful
//! `detail-type`, and one that did NOT under its unsuccessful one (`EC2 Instance Launch
//! Unsuccessful` for a launch that placed nothing) — carrying the terminal scaling activity
//! that recorded it (`AutoScalingGroupName`, `ActivityId`, the activity's `StatusCode`,
//! `StatusMessage`, `Description` and `Cause`) and naming in `resources` the group and, where
//! Amazon EC2 made an attempt and took it back, the instance of that attempt. The capacity
//! stays owed, so the group's OWN retry publishes the successful counterpart once the cause is
//! repaired, with no further Auto Scaling call from anybody.
//!
//! The rows span what the rule generalises over: WHY the launch placed nothing (a volume key
//! that produces no data key, in three key states and two spellings; a launch source that no
//! longer resolves against Amazon EC2), whether an ATTEMPT was minted for it at all (a launch
//! Amazon EC2 accepts and withdraws leaves one instance per unplaced unit; one it never
//! carried out leaves none), and which block device mapping of the version names the key. The
//! second test is the repair the rule turns on: the same group, polled only through the queue,
//! publishes the successful launch once the key is enabled again.
//!
//! Auto Scaling, Amazon EC2, KMS, EventBridge and Amazon SQS are mounted on ONE engine and
//! driven over their own wire protocols, so a publication travels the real in-process
//! callback, through the real bus, to a real SQS target: nothing here reads the publisher's
//! own queue.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_kms_trentservice as kms_service;

use crate::launch_volume_encryption_key::{
    authorize_role_on_key, create_group, create_key, create_launch_template, described_group,
    group_instances, kms, set_desired_capacity, Volume, DEADLINE, POLL_PAUSE, ROOT_DEVICE,
};
use crate::support::{
    default_service_linked_role_arn, ec2_query, json_rpc, list_members, p, query, tag_value,
    xml_unescape, REGION,
};

/// The `source` and the two `detail-type`s a launch step is published under, written out here
/// rather than imported, so a wrong constant in the implementation cannot make a row pass.
const EVENT_SOURCE: &str = "aws.autoscaling";
const LAUNCH_UNSUCCESSFUL: &str = "EC2 Instance Launch Unsuccessful";
const LAUNCH_SUCCESSFUL: &str = "EC2 Instance Launch Successful";

/// The terminal `ScalingActivityStatusCode` values this suite asserts, spelled as the model's
/// own `ScalingActivityStatusCode` enum does.
const STATUS_CANCELLED: &str = "Cancelled";
const STATUS_SUCCESSFUL: &str = "Successful";

/// The `Progress` a terminal activity reports: `Activity$Progress` is 0..100 and an activity
/// that has ended is complete.
const TERMINAL_PROGRESS: &str = "100";

/// The device a row's second block device mapping names, when the row names its key there.
const SECOND_DEVICE: &str = "/dev/sdb";

/// The rule that delivers every event of this service to the queue: which step an envelope
/// reports is read off the envelope, never off which rule delivered it.
const RULE: &str = "autoscaling-blocked-launches";

/// The queue every fixture of this suite delivers to.
const QUEUE: &str = "blocked-launch-events";

/// How often a poll re-reads the target queue.
const POLL: Duration = Duration::from_millis(25);

/// What the cancelled activity of a launch whose volume key will not serve reports, and what
/// the event reporting that launch carries as its `StatusMessage`.
///
/// Written out here rather than taken from either implementation: a case that re-used the
/// service's own literal would assert nothing about it. Fixed text for this failure class —
/// the instance never reached `InService` and was taken back under Amazon EC2's own reason for
/// a key that is in no state a cryptographic operation can use.
const UNUSABLE_KEY_MESSAGE: &str = "Instance became unhealthy while waiting for instance to be \
                                    in InService state. Termination Reason: \
                                    Client.InvalidKMSKey.InvalidState: The KMS key provided is \
                                    in an incorrect state";

/// How long a poll keeps looking for a publication the clock already owes: the lifecycle span
/// the service owes a transition, with ample margin for a loaded test machine. A poll waits
/// for the CONDITION and returns as soon as it holds, so the margin costs nothing when the
/// publication is prompt.
fn publication_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION + Duration::from_secs(90)
}

/// The same, for the publication the group's own RETRY of a withdrawn launch produces: the
/// next attempt falls due a span of its own after the last one, so the budget covers several
/// of those beside the margin above.
fn retry_budget() -> Duration {
    publication_budget() + autoscaling::state::WITHDRAWN_LAUNCH_RETRY * 4
}

/// One emulator: Auto Scaling, Amazon EC2, KMS, EventBridge and Amazon SQS over ONE shared
/// engine — the five services a published event of a key-blocked launch travels through.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(kms_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// One EventBridge call.
async fn events(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    json_rpc(router, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    json_rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// An emulator whose default bus delivers every Auto Scaling event to one SQS queue; answers
/// the router and that queue's url.
async fn fixture(case: &str) -> (MultiServiceRouter, String) {
    let router = emulator();
    let (status, body) = sqs(&router, "CreateQueue", json!({ "QueueName": QUEUE })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue expected 200, got {status}: {body}"
    );
    let url = body["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {body}"))
        .to_string();
    let (status, body) = sqs(
        &router,
        "GetQueueAttributes",
        json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes expected 200, got {status}: {body}"
    );
    let queue_arn = body["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {body}"))
        .to_string();

    // EventBridge delivers as its own service principal, so the queue grants it the
    // `sqs:SendMessage` a target delivery makes.
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        }],
    })
    .to_string();
    let (status, body) = sqs(
        &router,
        "SetQueueAttributes",
        json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetQueueAttributes expected 200, got {status}: {body}"
    );

    let (status, body) = events(
        &router,
        "PutRule",
        json!({
            "Name": RULE,
            "EventPattern": json!({ "source": [EVENT_SOURCE] }).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRule expected 200, got {status}: {body}"
    );
    let (status, body) = events(
        &router,
        "PutTargets",
        json!({ "Rule": RULE, "Targets": [{ "Id": "sqs-target", "Arn": queue_arn }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutTargets expected 200, got {status}: {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {body}"
    );
    (router, url)
}

/// Every envelope waiting on the queue right now.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let (status, body) = sqs(
        router,
        "ReceiveMessage",
        json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage expected 200, got {status}: {body}"
    );
    body["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            let raw = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(raw).unwrap_or(Value::Null)
        })
        .collect()
}

/// Poll the queue — and ONLY the queue — until one envelope of `detail_type` has been
/// delivered, or `budget` runs out; answer that envelope.
///
/// Nothing in this loop calls Auto Scaling, which is the point: a client that watches a group
/// through EventBridge makes no further call to the service that publishes. Everything drained
/// goes into `buffer`, so an envelope that arrives while another is being waited for is kept
/// rather than thrown away.
async fn await_event(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    detail_type: &str,
    budget: Duration,
    buffer: &mut Vec<Value>,
) -> Value {
    let deadline = Instant::now() + budget;
    loop {
        buffer.extend(drain(router, case, url).await);
        if let Some(found) = buffer
            .iter()
            .find(|event| event["detail-type"] == json!(detail_type))
        {
            return found.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected one {detail_type} envelope, got {buffer:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Assert no envelope of `detail_type` is among everything delivered so far.
fn assert_absent(case: &str, buffer: &[Value], detail_type: &str) {
    assert!(
        !buffer
            .iter()
            .any(|event| event["detail-type"] == json!(detail_type)),
        "case {case:?}: no {detail_type} envelope may be published, got {buffer:?}"
    );
}

/// The text of `member` in an envelope's `detail`.
fn detail_text(case: &str, event: &Value, member: &str) -> String {
    event["detail"][member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no detail.{member} in {event}"))
        .to_string()
}

/// The ARN an Amazon EC2 instance is named by in an envelope's `resources`, built from the
/// account and Region this emulator answers for rather than taken from the implementation.
fn instance_arn(instance_id: &str) -> String {
    format!(
        "arn:aws:ec2:{REGION}:{}:instance/{instance_id}",
        ::engine::emulator_account_id()
    )
}

/// A key ARN of this account and Region whose key id names no key at all: well formed, so KMS
/// resolves it rather than refusing it as malformed, and resolving it finds nothing.
fn absent_key_arn() -> String {
    format!(
        "arn:aws:kms:{REGION}:{}:key/00000000-1111-2222-3333-555555555555",
        ::engine::emulator_account_id()
    )
}

/// The `(StatusCode, StatusMessage, Progress)` of the group's activity `activity_id`, read
/// through `DescribeScalingActivities` the way a client correlates an event to the activity it
/// names — or `None` when the group's log holds no such activity.
async fn activity(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    activity_id: &str,
) -> Option<(String, String, String)> {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeScalingActivities expected 200, got {status}: {body}"
    );
    list_members(&body, "Activities")
        .into_iter()
        .find(|member| tag_value(member, "ActivityId") == activity_id)
        .map(|member| {
            (
                tag_value(member, "StatusCode"),
                xml_unescape(&tag_value(member, "StatusMessage")),
                tag_value(member, "Progress"),
            )
        })
}

/// Why the launch of a row places nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cause {
    /// A customer managed key the client disabled, named on the root volume by its key id.
    DisabledKeyById,
    /// The same key state, named on the root volume by the key ARN KMS minted.
    DisabledKeyByArn,
    /// A customer managed key scheduled for deletion, named on a SECOND volume of the version.
    KeyPendingDeletionOnSecondVolume,
    /// A well-formed key ARN of this account and Region that names no key.
    AbsentKeyArn,
    /// The launch template itself, deleted after the group was configured against it: a launch
    /// source Amazon EC2 no longer resolves, so no attempt is ever made.
    DeletedLaunchTemplate,
}

impl Cause {
    /// Whether Amazon EC2 ACCEPTS such a launch and takes the instance back — which is what
    /// leaves one attempt per unplaced unit for the event to name.
    fn mints_an_attempt(self) -> bool {
        self != Self::DeletedLaunchTemplate
    }
}

/// One row of the table.
struct Row {
    case: &'static str,
    group: &'static str,
    template: &'static str,
    cause: Cause,
}

const ROWS: &[Row] = &[
    Row {
        case: "a disabled key named by its key id on the root volume",
        group: "blocked-events-disabled-id",
        template: "lt-blocked-events-disabled-id",
        cause: Cause::DisabledKeyById,
    },
    Row {
        case: "a disabled key named by its key ARN on the root volume",
        group: "blocked-events-disabled-arn",
        template: "lt-blocked-events-disabled-arn",
        cause: Cause::DisabledKeyByArn,
    },
    Row {
        case: "a key scheduled for deletion, named on a second volume",
        group: "blocked-events-pending-deletion",
        template: "lt-blocked-events-pending-deletion",
        cause: Cause::KeyPendingDeletionOnSecondVolume,
    },
    Row {
        case: "a well-formed key ARN naming no key of the account",
        group: "blocked-events-absent-key",
        template: "lt-blocked-events-absent-key",
        cause: Cause::AbsentKeyArn,
    },
    Row {
        case: "the launch template deleted under the group",
        group: "blocked-events-deleted-template",
        template: "lt-blocked-events-deleted-template",
        cause: Cause::DeletedLaunchTemplate,
    },
];

/// Create the launch source one row launches from and the group that launches from it, and
/// answer `(group ARN, the StatusMessage the blocked launch must report)`.
async fn given_blocked_group(router: &MultiServiceRouter, row: &Row) -> (String, String) {
    let case = row.case;
    let role_arn = default_service_linked_role_arn();
    // The key the row's volume names, where it names one, and the sentence the launch it
    // blocks must report.
    let (reference, message) = match row.cause {
        Cause::DisabledKeyById | Cause::DisabledKeyByArn => {
            let (key_id, key_arn) = create_key(router, case).await;
            authorize_role_on_key(router, case, &key_id, &role_arn).await;
            kms(router, case, "DisableKey", json!({ "KeyId": &key_id })).await;
            let reference = if row.cause == Cause::DisabledKeyById {
                key_id
            } else {
                key_arn
            };
            (Some(reference), UNUSABLE_KEY_MESSAGE.to_string())
        }
        Cause::KeyPendingDeletionOnSecondVolume => {
            let (key_id, _) = create_key(router, case).await;
            authorize_role_on_key(router, case, &key_id, &role_arn).await;
            kms(
                router,
                case,
                "ScheduleKeyDeletion",
                json!({ "KeyId": &key_id, "PendingWindowInDays": 7 }),
            )
            .await;
            (Some(key_id), UNUSABLE_KEY_MESSAGE.to_string())
        }
        Cause::AbsentKeyArn => (Some(absent_key_arn()), UNUSABLE_KEY_MESSAGE.to_string()),
        Cause::DeletedLaunchTemplate => (None, String::new()),
    };
    let volumes = if row.cause == Cause::KeyPendingDeletionOnSecondVolume {
        vec![
            Volume {
                device: ROOT_DEVICE,
                kms_key_id: None,
            },
            Volume {
                device: SECOND_DEVICE,
                kms_key_id: reference.as_deref(),
            },
        ]
    } else {
        vec![Volume {
            device: ROOT_DEVICE,
            kms_key_id: reference.as_deref(),
        }]
    };
    let (template_id, _latest) = create_launch_template(router, case, row.template, &volumes).await;
    create_group(router, case, row.group, &template_id, "$Default").await;
    // The launch source of the row that has none: the template is the client's to delete, and
    // the group resolves it at the instant of each launch.
    let message = if row.cause == Cause::DeletedLaunchTemplate {
        let (status, body) = ec2_query(
            router,
            "DeleteLaunchTemplate",
            &[p("LaunchTemplateId", &template_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteLaunchTemplate expected 200, got {status}: {body}"
        );
        // Amazon EC2's own sentence for a LAUNCH naming a template the account no longer
        // holds, written out here rather than taken from either implementation.
        format!(
            "The specified launch template, with template ID {template_id}, does not exist."
        )
    } else {
        message
    };
    let described = described_group(router, case, row.group).await;
    let arn = tag_value(&described, "AutoScalingGroupARN");
    assert!(
        !arn.is_empty(),
        "case {case:?}: the group must report its ARN, got {described}"
    );
    (arn, message)
}

/// A capacity change whose launch places nothing publishes that step under the launch's
/// UNSUCCESSFUL detail-type, carrying the terminal activity that recorded it and naming the
/// group — and the withdrawn attempt where one was made.
#[tokio::test]
async fn a_launch_that_places_nothing_is_published_as_unsuccessful() {
    for row in ROWS {
        let case = row.case;
        let (router, url) = fixture(case).await;
        let (group_arn, expected_message) = given_blocked_group(&router, row).await;

        set_desired_capacity(&router, case, row.group, "1").await;

        let mut buffer: Vec<Value> = Vec::new();
        let event = await_event(
            &router,
            case,
            &url,
            LAUNCH_UNSUCCESSFUL,
            publication_budget(),
            &mut buffer,
        )
        .await;

        assert_eq!(
            event["source"],
            json!(EVENT_SOURCE),
            "case {case:?}: the step must be published by this service, got {event}"
        );
        assert_eq!(
            detail_text(case, &event, "AutoScalingGroupName"),
            row.group,
            "case {case:?}: the event must name the group the step belonged to, got {event}"
        );
        assert_eq!(
            detail_text(case, &event, "StatusCode"),
            STATUS_CANCELLED,
            "case {case:?}: a launch Amazon EC2 did not carry through is cancelled, got {event}"
        );
        assert_eq!(
            detail_text(case, &event, "StatusMessage"),
            expected_message,
            "case {case:?}: the event must carry the reason the launch placed nothing, got \
             {event}"
        );
        assert!(
            !detail_text(case, &event, "Cause").is_empty(),
            "case {case:?}: the event must carry the activity's cause, got {event}"
        );

        // The attempt the step made, where Amazon EC2 made one: the event names it, and so
        // does the activity's description.
        let instance_id = detail_text(case, &event, "EC2InstanceId");
        let description = detail_text(case, &event, "Description");
        if row.cause.mints_an_attempt() {
            assert!(
                instance_id.starts_with("i-"),
                "case {case:?}: a launch Amazon EC2 took back names the attempt it minted, got \
                 {event}"
            );
            assert!(
                description.contains(&instance_id),
                "case {case:?}: the activity's description must name the instance the event \
                 does, got {event}"
            );
            assert_eq!(
                event["resources"],
                json!([group_arn, instance_arn(&instance_id)]),
                "case {case:?}: the envelope must name the group and the attempt, got {event}"
            );
        } else {
            assert!(
                instance_id.is_empty(),
                "case {case:?}: a launch no attempt was made for names no instance, got {event}"
            );
            assert_eq!(
                event["resources"],
                json!([group_arn]),
                "case {case:?}: the envelope of a launch with no attempt names the group alone, \
                 got {event}"
            );
        }

        // The event reports the activity a read of the same instant reports — never a second,
        // invented copy of it.
        let activity_id = detail_text(case, &event, "ActivityId");
        let recorded = activity(&router, case, row.group, &activity_id).await;
        assert_eq!(
            recorded,
            Some((
                STATUS_CANCELLED.to_string(),
                expected_message.clone(),
                TERMINAL_PROGRESS.to_string(),
            )),
            "case {case:?}: detail.ActivityId {activity_id} must name the group's own terminal \
             activity, got {recorded:?}"
        );

        // Nothing claimed a launch that did not happen, and the capacity the caller asked for
        // STANDS: it is the capacity the group maintains, not an instruction one call carried
        // out.
        assert_absent(case, &buffer, LAUNCH_SUCCESSFUL);
        let instances = group_instances(&router, case, row.group).await;
        assert!(
            instances.is_empty(),
            "case {case:?}: the group must hold no instance for a launch that placed nothing, \
             got {instances:?}"
        );
        let described = described_group(&router, case, row.group).await;
        assert_eq!(
            tag_value(&described, "DesiredCapacity"),
            "1",
            "case {case:?}: the group must keep the capacity that was asked for, got {described}"
        );
    }
}

/// The capacity a cancelled launch could not place stays OWED: once the key is enabled again
/// the group's own retry launches it and publishes the SUCCESSFUL counterpart on the same rule,
/// with no Auto Scaling call in between.
#[tokio::test]
async fn the_group_publishes_the_successful_launch_once_the_key_is_enabled() {
    let case = "a disabled key enabled again while the group is watched through the queue";
    let group = "blocked-events-key-repaired";
    let template = "lt-blocked-events-key-repaired";
    let (router, url) = fixture(case).await;

    let (key_id, _) = create_key(&router, case).await;
    authorize_role_on_key(&router, case, &key_id, &default_service_linked_role_arn()).await;
    kms(&router, case, "DisableKey", json!({ "KeyId": &key_id })).await;
    let (template_id, _latest) = create_launch_template(
        &router,
        case,
        template,
        &[Volume {
            device: ROOT_DEVICE,
            kms_key_id: Some(&key_id),
        }],
    )
    .await;
    create_group(&router, case, group, &template_id, "$Default").await;
    set_desired_capacity(&router, case, group, "1").await;

    let mut buffer: Vec<Value> = Vec::new();
    let cancelled = await_event(
        &router,
        case,
        &url,
        LAUNCH_UNSUCCESSFUL,
        publication_budget(),
        &mut buffer,
    )
    .await;
    let withdrawn = detail_text(case, &cancelled, "EC2InstanceId");
    assert_absent(case, &buffer, LAUNCH_SUCCESSFUL);

    // The client's own remediation, made on the service that owns the key. Nothing asks Auto
    // Scaling for the capacity again.
    kms(&router, case, "EnableKey", json!({ "KeyId": &key_id })).await;

    let launched = await_event(
        &router,
        case,
        &url,
        LAUNCH_SUCCESSFUL,
        retry_budget(),
        &mut buffer,
    )
    .await;
    assert_eq!(
        detail_text(case, &launched, "AutoScalingGroupName"),
        group,
        "case {case:?}: the successful launch must name the same group, got {launched}"
    );
    assert_eq!(
        detail_text(case, &launched, "StatusCode"),
        STATUS_SUCCESSFUL,
        "case {case:?}: a launch that carried through is successful, got {launched}"
    );
    assert!(
        detail_text(case, &launched, "StatusMessage").is_empty(),
        "case {case:?}: a step that carried through states no reason, got {launched}"
    );
    let instance_id = detail_text(case, &launched, "EC2InstanceId");
    assert!(
        instance_id.starts_with("i-") && instance_id != withdrawn,
        "case {case:?}: the retry must publish a NEW instance, got {instance_id:?} against the \
         withdrawn {withdrawn:?}"
    );

    // And the group holds exactly that instance, in service.
    let deadline = Instant::now() + DEADLINE;
    let instances = loop {
        let instances = group_instances(&router, case, group).await;
        if instances
            .iter()
            .any(|(_, state, _)| state == "InService")
            || Instant::now() >= deadline
        {
            break instances;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    };
    assert_eq!(
        instances.len(),
        1,
        "case {case:?}: the group must hold the one instance its retry launched, got {instances:?}"
    );
    assert_eq!(
        (instances[0].0.as_str(), instances[0].1.as_str()),
        (instance_id.as_str(), "InService"),
        "case {case:?}: the group must report the published instance in service, got {instances:?}"
    );
    let described = described_group(&router, case, group).await;
    assert_eq!(
        tag_value(&described, "DesiredCapacity"),
        "1",
        "case {case:?}: the group must still hold the capacity that was asked for, got {described}"
    );
}
