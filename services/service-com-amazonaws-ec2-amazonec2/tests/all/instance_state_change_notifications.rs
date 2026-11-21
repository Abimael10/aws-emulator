// SPDX-License-Identifier: CC0-1.0

//! The event every change of an INSTANCE's state is published as, end to end over the wire.
//!
//! THE RULE: Amazon EC2 is an event source as well as a control plane — whenever an instance
//! changes state, that change is published onto the account's default EventBridge bus as
//! `source` `aws.ec2` / `detail-type` `EC2 Instance State-change Notification`, naming the
//! instance in the envelope's `resources` and carrying in the event `detail` the instance's id
//! and the state it moved INTO. Every move is published, whatever moved it: the launch's own
//! `pending` and the `running` the clock owes it, the `stopping` and `stopped` of a stop, the
//! `pending` and `running` of a start, and the `shutting-down` and `terminated` of a
//! termination — including the `terminated` an instance is owed when the KMS key its volumes
//! name will not produce their data key. So anything that reacts to an instance's lifecycle
//! learns of it without polling Amazon EC2, and a client waiting for the notification is told
//! instead of hanging.
//!
//! The rows span what the rule generalises over: WHICH call moved the instance (a launch, a
//! stop, a start, a termination) and the OUTCOME the move leads to (an instance that comes up,
//! and one that is taken back out because its root volume's KMS key will not serve it).
//!
//! Nothing here reads this crate's own state: AWS KMS, EventBridge and Amazon SQS are mounted
//! BESIDE Amazon EC2 on one engine, exactly as the assembled router mounts them, every key is
//! minted and moved through KMS's own API, and every event is read off the queue a real rule
//! on the real default bus delivered it to.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_eventbridge_awsevents as eventbridge;
use service_com_amazonaws_kms_trentservice as kms;
use service_com_amazonaws_sqs_amazonsqs as sqs;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, indexed};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// An AMI id of the right FORM that this account holds no image for: the launch is still
/// served, and its root device is the one the image this crate publishes by default carries.
const AMI: &str = "ami-0123456789abcdef0";

/// The `source` and `detail-type` an instance's state change is published under, spelled out
/// here rather than imported, so a wrong constant in the implementation cannot make a test
/// pass.
const EC2_EVENT_SOURCE: &str = "aws.ec2";
const INSTANCE_STATE_CHANGE: &str = "EC2 Instance State-change Notification";

/// How long a poll keeps asking for the events of one move before giving up. Generous: the
/// publication is made once the move falls due, so this is many times the work a row does and
/// still lets a broken case fail rather than hang.
const DELIVERY_BUDGET: Duration = Duration::from_secs(60);

/// How long a poll waits between asks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How many `ReceiveMessage` rounds one drain makes: a receive answers at most ten messages,
/// and this is well above the events the whole table publishes.
const MAX_RECEIVE_ROUNDS: usize = 8;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// Amazon EC2, AWS KMS, EventBridge and Amazon SQS over ONE shared engine, exactly as the
/// assembled router mounts them: the key a root volume names, the bus an instance's moves are
/// published onto and the queue a rule delivers those events to are each that service's own
/// resource.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(kms::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(eventbridge::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(sqs::EmulatedService::new(engine));
    router
}

/// One `awsJson` RPC call against the service whose signing name is `signing_name`, answering
/// the status and the payload.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDINSTANCEEVENTS/20260101/{REGION}/\
                 {signing_name}/aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
            ),
        )
        .body(::bytes::Bytes::from(
            ::serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed JSON request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        ::serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// One AWS KMS call, which must be served.
async fn kms_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, payload) = json_call(router, "kms", "TrentService", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: KMS {operation} must answer 200, got {status} with {payload}"
    );
    payload
}

/// One EventBridge call, which must be served.
async fn events_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, payload) = json_call(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EventBridge {operation} must answer 200, got {status} with {payload}"
    );
    payload
}

/// One Amazon SQS call, which must be served.
async fn sqs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, payload) = json_call(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Amazon SQS {operation} must answer 200, got {status} with {payload}"
    );
    payload
}

/// One EC2 call, which must be served, as its body.
async fn ec2_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One customer managed key KMS minted, answering its key id.
async fn key(router: &MultiServiceRouter, case: &str) -> String {
    let created = kms_ok(router, case, "CreateKey", json!({})).await;
    created["KeyMetadata"]["KeyId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateKey reports the key it minted, got {created}")
        })
        .to_string()
}

/// A queue, created through Amazon SQS itself and carrying the grant a rule's target needs —
/// EventBridge makes the target's `sqs:SendMessage` as its own service principal and relies on
/// the queue's resource-based policy — answering its url and the ARN a rule names it by.
async fn queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs_ok(router, case, "CreateQueue", json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateQueue must answer a QueueUrl, got {created}")
        })
        .to_string();
    let attributes = sqs_ok(
        router,
        case,
        "GetQueueAttributes",
        json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: GetQueueAttributes must answer a QueueArn, got {attributes}")
        })
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs_ok(
        router,
        case,
        "SetQueueAttributes",
        json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// A default-bus rule matching every `aws.ec2` instance state change, delivering to a queue of
/// its own, and that queue's url — the ONE wiring the tests below watch the events through.
async fn watched_queue(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (url, queue_arn) = queue(router, case, name).await;
    events_ok(
        router,
        case,
        "PutRule",
        json!({
            "Name": name,
            "State": "ENABLED",
            "EventPattern": json!({
                "source": [EC2_EVENT_SOURCE],
                "detail-type": [INSTANCE_STATE_CHANGE],
            })
            .to_string(),
        }),
    )
    .await;
    events_ok(
        router,
        case,
        "PutTargets",
        json!({ "Rule": name, "Targets": [{ "Id": "t1", "Arn": queue_arn }] }),
    )
    .await;
    url
}

/// The events delivered to one queue, filed by the instance each is about.
///
/// The moves of several instances interleave on one queue, so a drain files every event it
/// takes rather than discarding the ones another row is waiting for.
struct Watcher {
    url: String,
    by_instance: BTreeMap<String, Vec<Value>>,
}

impl Watcher {
    /// Take everything waiting on the queue right now and file it.
    async fn drain(&mut self, router: &MultiServiceRouter, case: &str) {
        for _ in 0..MAX_RECEIVE_ROUNDS {
            let received = sqs_ok(
                router,
                case,
                "ReceiveMessage",
                json!({ "QueueUrl": &self.url, "MaxNumberOfMessages": 10, "WaitTimeSeconds": 0 }),
            )
            .await;
            let messages = received["Messages"].as_array().cloned().unwrap_or_default();
            if messages.is_empty() {
                break;
            }
            for message in messages {
                if let Some(handle) = message["ReceiptHandle"].as_str() {
                    sqs_ok(
                        router,
                        case,
                        "DeleteMessage",
                        json!({ "QueueUrl": &self.url, "ReceiptHandle": handle }),
                    )
                    .await;
                }
                let body = message["Body"].as_str().unwrap_or_default();
                let event: Value = ::serde_json::from_str(body).unwrap_or_else(|error| {
                    panic!("case {case:?}: a delivered event is JSON ({error}): {body}")
                });
                let instance_id = event["detail"]["instance-id"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: an event must name its instance, got {event}")
                    })
                    .to_string();
                self.by_instance.entry(instance_id).or_default().push(event);
            }
        }
    }

    /// The events delivered for `instance_id`, once at least `expected` of them have arrived.
    ///
    /// Looked for until the deadline passes rather than waited out, so the answer is the one
    /// the service published however fast the host runs.
    async fn events_for(
        &mut self,
        router: &MultiServiceRouter,
        case: &str,
        instance_id: &str,
        expected: usize,
    ) -> Vec<Value> {
        let deadline = Instant::now() + DELIVERY_BUDGET;
        loop {
            self.drain(router, case).await;
            let delivered = self
                .by_instance
                .get(instance_id)
                .map(Vec::len)
                .unwrap_or_default();
            if delivered >= expected {
                return self.by_instance.remove(instance_id).unwrap_or_default();
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: {expected} state changes of {instance_id} must be delivered, \
                 got {:?}",
                self.states_of(instance_id)
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// The states filed for `instance_id` so far, in delivery order.
    fn states_of(&self, instance_id: &str) -> Vec<String> {
        self.by_instance
            .get(instance_id)
            .map(|events| {
                events
                    .iter()
                    .map(|event| event["detail"]["state"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The states `events` report, in the order they were delivered, having pinned the envelope
/// every one of them carries: the source and detail-type of an instance state change, and an
/// envelope naming THIS instance in `resources`.
fn states(case: &str, instance_id: &str, events: &[Value]) -> Vec<String> {
    for event in events {
        assert_eq!(
            event["source"].as_str(),
            Some(EC2_EVENT_SOURCE),
            "case {case:?}: every state change carries the EC2 source, got {event}"
        );
        assert_eq!(
            event["detail-type"].as_str(),
            Some(INSTANCE_STATE_CHANGE),
            "case {case:?}: every state change carries the instance detail-type, got {event}"
        );
        let names_instance = event["resources"].as_array().is_some_and(|resources| {
            resources.iter().any(|resource| {
                resource
                    .as_str()
                    .is_some_and(|arn| arn.ends_with(&format!("instance/{instance_id}")))
            })
        });
        assert!(
            names_instance,
            "case {case:?}: every state change names {instance_id} in its resources, got {event}"
        );
    }
    events
        .iter()
        .map(|event| {
            event["detail"]["state"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The `BlockDeviceMapping.1` fields that name `device` with the Amazon EBS members `members`
/// carries.
fn mapping(device: &str, members: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut params = vec![p("BlockDeviceMapping.1.DeviceName", device)];
    for (member, value) in members {
        params.push(p(&format!("BlockDeviceMapping.1.Ebs.{member}"), *value));
    }
    params
}

/// One `RunInstances` of a single instance from [`AMI`] with `extra` beside it, answering the
/// instance it minted.
async fn launch(router: &MultiServiceRouter, case: &str, extra: &[(String, String)]) -> String {
    let mut params = vec![
        p("ImageId", AMI),
        p("MinCount", "1"),
        p("MaxCount", "1"),
    ];
    params.extend_from_slice(extra);
    let body = ec2_ok(router, case, "RunInstances", &params).await;
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: the launch must report its instance: {body}"))
}

/// The root device name an instance launched from [`AMI`] reports, read off a launch rather
/// than written here: it is the AMI's own `rootDeviceName`.
async fn root_device(router: &MultiServiceRouter, case: &str) -> String {
    let instance_id = launch(router, case, &[]).await;
    let described = ec2_ok(
        router,
        case,
        "DescribeInstances",
        &indexed("InstanceId", &[&instance_id]),
    )
    .await;
    element(&described, "rootDeviceName").unwrap_or_else(|| {
        panic!("case {case:?}: the instance must report its root device: {described}")
    })
}

/// Look at `instance_id` until it reports `state`, so a call that acts on an instance in a
/// particular state is made then and not before.
async fn until_state(router: &MultiServiceRouter, case: &str, instance_id: &str, state: &str) {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let body = ec2_ok(
            router,
            case,
            "DescribeInstances",
            &indexed("InstanceId", &[instance_id]),
        )
        .await;
        if elements(&body, "name").iter().any(|name| name == state) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {instance_id} must reach {state}, got {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the launch table: the case, the Amazon EBS members the launch's root device
/// entry carries, and the states the instance must announce, in order.
type LaunchRow = (&'static str, Vec<(&'static str, String)>, Vec<&'static str>);

/// THE RULE over the moves a LAUNCH makes: the `pending` the launch answers and the state the
/// clock owes the instance — `running` for a launch whose root volume can be created, and the
/// shutdown through `shutting-down` to `terminated` for one whose root volume's key will not
/// serve it.
#[tokio::test]
async fn a_launch_publishes_every_state_its_instance_moves_into() {
    let router = emulator();
    let url = watched_queue(&router, "a launch", "instance-moves").await;
    let mut watcher = Watcher {
        url,
        by_instance: BTreeMap::new(),
    };

    let serving = key(&router, "a key KMS will produce a data key under").await;
    let disabled = key(&router, "a disabled key").await;
    kms_ok(
        &router,
        "a disabled key",
        "DisableKey",
        json!({ "KeyId": disabled.clone() }),
    )
    .await;
    let device = root_device(&router, "the AMI's own root device").await;

    // Each row names the Amazon EBS members its launch's root device entry carries and the
    // states the instance it mints must announce, in order.
    let rows: Vec<LaunchRow> = vec![
        (
            "a launch with no encrypted root volume at all",
            Vec::new(),
            vec!["pending", "running"],
        ),
        (
            "a launch whose root volume's key will serve it",
            vec![("Encrypted", "true".to_string()), ("KmsKeyId", serving)],
            vec!["pending", "running"],
        ),
        (
            "a launch whose root volume's key is disabled",
            vec![
                ("Encrypted", "true".to_string()),
                ("KmsKeyId", disabled.clone()),
            ],
            vec!["pending", "shutting-down", "terminated"],
        ),
    ];

    for (case, members, expected) in &rows {
        let borrowed: Vec<(&str, &str)> = members
            .iter()
            .map(|(member, value)| (*member, value.as_str()))
            .collect();
        let extra = if borrowed.is_empty() {
            Vec::new()
        } else {
            mapping(&device, &borrowed)
        };
        let instance_id = launch(&router, case, &extra).await;
        let delivered = watcher
            .events_for(&router, case, &instance_id, expected.len())
            .await;
        let announced = states(case, &instance_id, &delivered);
        assert_eq!(
            announced, *expected,
            "case {case:?}: expected {expected:?} for {instance_id}, got {announced:?}"
        );
    }
}

/// THE RULE over the moves a CALLER makes: a stop, a start and a termination each publish the
/// state they put the instance in and the state the clock then owes it.
#[tokio::test]
async fn a_stop_a_start_and_a_termination_publish_their_own_moves() {
    let router = emulator();
    let case = "the moves a caller makes";
    let url = watched_queue(&router, case, "caller-moves").await;
    let mut watcher = Watcher {
        url,
        by_instance: BTreeMap::new(),
    };

    let instance_id = launch(&router, case, &[]).await;
    // The launch's own moves, taken off the queue so each call below is read on its own.
    let launched = watcher.events_for(&router, case, &instance_id, 2).await;
    assert_eq!(
        states(case, &instance_id, &launched),
        vec!["pending", "running"],
        "case {case:?}: the launch announces its own two moves"
    );

    until_state(&router, case, &instance_id, "running").await;
    ec2_ok(
        &router,
        case,
        "StopInstances",
        &indexed("InstanceId", &[&instance_id]),
    )
    .await;
    let stopped = watcher.events_for(&router, case, &instance_id, 2).await;
    assert_eq!(
        states(case, &instance_id, &stopped),
        vec!["stopping", "stopped"],
        "case {case:?}: a stop announces both of its moves"
    );

    until_state(&router, case, &instance_id, "stopped").await;
    ec2_ok(
        &router,
        case,
        "StartInstances",
        &indexed("InstanceId", &[&instance_id]),
    )
    .await;
    let started = watcher.events_for(&router, case, &instance_id, 2).await;
    assert_eq!(
        states(case, &instance_id, &started),
        vec!["pending", "running"],
        "case {case:?}: a start announces both of its moves"
    );

    until_state(&router, case, &instance_id, "running").await;
    ec2_ok(
        &router,
        case,
        "TerminateInstances",
        &indexed("InstanceId", &[&instance_id]),
    )
    .await;
    let terminated = watcher.events_for(&router, case, &instance_id, 2).await;
    assert_eq!(
        states(case, &instance_id, &terminated),
        vec!["shutting-down", "terminated"],
        "case {case:?}: a termination announces both of its moves"
    );

    // A second termination moves the instance nowhere, so it announces nothing: the events are
    // about an instance's moves, not about the calls made.
    ec2_ok(
        &router,
        case,
        "TerminateInstances",
        &indexed("InstanceId", &[&instance_id]),
    )
    .await;
    watcher.drain(&router, case).await;
    let repeated = watcher.states_of(&instance_id);
    assert!(
        repeated.is_empty(),
        "case {case:?}: a repeated termination announces nothing, got {repeated:?}"
    );
}
