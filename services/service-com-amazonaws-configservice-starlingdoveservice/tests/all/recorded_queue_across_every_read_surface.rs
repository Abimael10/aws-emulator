// SPDX-License-Identifier: CC0-1.0

//! What a RECORDING configuration recorder produces for a resource that lives in another
//! service, read back through every configuration-item surface Config serves.
//!
//! Two rules are pinned here, each over a TABLE of inputs rather than one recorded call:
//!
//! 1. While a configuration recorder is recording and its recording group covers a
//!    resource type, Config records a configuration item for each resource of that type
//!    the owning service holds — on discovery, on each state it changes into and on its
//!    deletion — and EVERY configuration-item read projects that one inventory:
//!    `GetResourceConfigHistory` reports the resource's items, `BatchGetResourceConfig`
//!    answers the recorded keys of a batch and leaves a key it holds no item for out of
//!    both of its lists, `SelectResourceConfig` answers a row per recorded resource the
//!    query matches, and a well-formed key the account never recorded is refused
//!    `ResourceNotDiscoveredException`.
//! 2. A recorder that is RECORDING records whatever turned it on: the recorder its linked
//!    service creates is recording from the instant it was created — the caller can make
//!    no start call for it (`UnmodifiableEntityException`: "Service-linked configuration
//!    recorders are always recording") — so the account's resources are recorded for it
//!    with no further call.
//!
//! Nothing below calls `PutResourceConfig`: every configuration item asserted on is one
//! the recorder OBSERVED through the owning service's own API, and every queue is created
//! and changed through Amazon SQS. Config and Amazon SQS are mounted on ONE engine, which
//! is what the `config_recorded_resource_compliance` harness builds.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::config_recorded_resource_compliance::{Emulator, ACCOUNT_ID, REGION};

/// The resource type Config records an Amazon SQS queue under, an entry of
/// `com.amazonaws.configservice#ResourceType` from
/// `smithy_query(query="shape", shape="ResourceType")`.
const QUEUE_RESOURCE_TYPE: &str = "AWS::SQS::Queue";

/// `ConfigurationItemStatus` values, from
/// `smithy_query(query="shape", shape="ConfigurationItemStatus")` (whose values are `OK`,
/// `ResourceDiscovered`, `ResourceNotRecorded`, `ResourceDeleted` and
/// `ResourceDeletedNotRecorded`): the status of the item that first discovered a resource,
/// and the status of the final item recording that it is gone.
const RESOURCE_DISCOVERED: &str = "ResourceDiscovered";
/// See [`RESOURCE_DISCOVERED`].
const RESOURCE_DELETED: &str = "ResourceDeleted";

/// The error a configuration-item read answers for a resource key the account has
/// recorded no configuration item for.
const NOT_DISCOVERED: &str = "ResourceNotDiscoveredException";

/// The service principals whose linked service owns a service-linked configuration
/// recorder, from `state::SERVICE_LINKED_PRINCIPALS` — the list
/// `PutServiceLinkedConfigurationRecorder`'s own documentation points at. Both are used
/// below, so the rule is pinned on the entry the issue did not cite as well.
const SERVICE_LINKED_PRINCIPALS: [&str; 2] = [
    "observabilityadmin.amazonaws.com",
    "telemetry-enablement.observabilityadmin.amazonaws.com",
];

/// How long a poll waits for the recording pass the clock owes. A pass falls due a
/// fraction of a second after the call that makes it due, so this is hundreds of passes'
/// worth of slack and not a span the behaviour depends on: the wait ends on the CONDITION,
/// and the budget is only there so a wedged emulator fails instead of hanging.
const CONVERGENCE_BUDGET: Duration = Duration::from_secs(180);

/// How long a poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A well-formed queue url of this account that NO queue was ever created for: the form
/// Amazon SQS names a queue by (`https://sqs.<region>.amazonaws.com/<accountId>/<name>`),
/// so the key is well formed and the account simply holds nothing for it.
fn never_created_queue_url(name: &str) -> String {
    format!("https://sqs.{REGION}.amazonaws.com/{ACCOUNT_ID}/{name}")
}

/// The configuration items `GetResourceConfigHistory` reports for `queue_url`, newest
/// first — empty while Config has recorded no item for the key, which that read answers by
/// refusing.
async fn history(case: &str, emulator: &Emulator, queue_url: &str) -> Vec<Value> {
    let (status, body) = emulator
        .call(
            "config",
            "StarlingDoveService",
            "1.1",
            "GetResourceConfigHistory",
            &json!({ "resourceType": QUEUE_RESOURCE_TYPE, "resourceId": queue_url }),
        )
        .await;
    if status == 400 && body["__type"] == json!(NOT_DISCOVERED) {
        return Vec::new();
    }
    assert_eq!(
        status, 200,
        "case {case:?}: GetResourceConfigHistory must answer 200, got {status} with {body}"
    );
    body["configurationItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Read `queue_url`'s history until `settled` holds of the items it reports, and answer
/// them.
///
/// What is waited for is the pass the recorder owes, never a span: the assertion that
/// fails at the deadline names the items the read did report.
async fn await_history<P>(
    case: &str,
    emulator: &Emulator,
    queue_url: &str,
    wanted: &str,
    settled: P,
) -> Vec<Value>
where
    P: Fn(&[Value]) -> bool,
{
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let items = history(case, emulator, queue_url).await;
        if settled(&items) {
            return items;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {wanted} for {queue_url}, got {items:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `configurationItemStatus` of each item, newest first.
fn statuses(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|item| {
            item["configurationItemStatus"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The recorded `configuration` document of `item`, decoded from the JSON string that
/// member carries.
fn configuration(case: &str, item: &Value) -> Value {
    let encoded = item["configuration"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a recorded configuration, got {item}"));
    serde_json::from_str(encoded).unwrap_or_else(|error| {
        panic!("case {case:?}: a recorded configuration must be JSON, got {error} for {encoded}")
    })
}

/// The value the item records for `attribute` in its configuration document, or `None`
/// when the item carries no such attribute.
fn recorded_value(item: &Value, attribute: &str) -> Option<String> {
    let encoded = item["configuration"].as_str()?;
    let document: Value = serde_json::from_str(encoded).ok()?;
    document[attribute].as_str().map(str::to_string)
}

/// Create a queue through Amazon SQS' own API and answer its url.
async fn create_queue(case: &str, emulator: &Emulator, name: &str, attributes: &Value) -> String {
    let mut request = json!({ "QueueName": name });
    if !attributes.is_null() {
        request["Attributes"] = attributes.clone();
    }
    let created = emulator.sqs("CreateQueue", &request).await;
    created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue must answer a QueueUrl, got {created}"))
        .to_string()
}

/// The attribute Amazon SQS reports for `queue_url`, as the live value a recorded item is
/// compared against.
async fn live_attribute(case: &str, emulator: &Emulator, queue_url: &str, attribute: &str) -> Value {
    let live = emulator
        .sqs(
            "GetQueueAttributes",
            &json!({ "QueueUrl": queue_url, "AttributeNames": ["All"] }),
        )
        .await;
    let value = live["Attributes"][attribute].clone();
    assert!(
        !value.is_null(),
        "case {case:?}: Amazon SQS must report {attribute} for {queue_url}, got {live}"
    );
    value
}

/// `BatchGetResourceConfig` over `keys`, each a `(resourceType, resourceId)` pair.
async fn batch_get(emulator: &Emulator, keys: &[(&str, &str)]) -> Value {
    let resource_keys: Vec<Value> = keys
        .iter()
        .map(|(resource_type, resource_id)| {
            json!({ "resourceType": resource_type, "resourceId": resource_id })
        })
        .collect();
    emulator
        .config(
            "BatchGetResourceConfig",
            &json!({ "resourceKeys": resource_keys }),
        )
        .await
}

/// The `resourceId` of every item a batched read reported.
fn batched_ids(case: &str, body: &Value) -> Vec<String> {
    body["baseConfigurationItems"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected baseConfigurationItems, got {body}"))
        .iter()
        .map(|item| {
            item["resourceId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: every batched item names its resourceId, got {body}")
                })
                .to_string()
        })
        .collect()
}

/// The rows every table below is read over: the queue's name and the attributes it is
/// created with, which is the whole of what the recorded configuration item is built from.
/// They differ along what the rule generalises over — how many attributes the queue
/// carries, whether it is a FIFO queue (which carries the attributes only a FIFO queue
/// has) and which attribute a change moves.
const QUEUES: [(&str, &str, &str, &str); 4] = [
    ("a queue created with no attributes", "surfaces-plain", "VisibilityTimeout", "37"),
    (
        "a queue created with one attribute",
        "surfaces-delayed",
        "DelaySeconds",
        "11",
    ),
    (
        "a queue created with several attributes",
        "surfaces-configured",
        "MessageRetentionPeriod",
        "600",
    ),
    ("a FIFO queue", "surfaces-ordered.fifo", "VisibilityTimeout", "53"),
];

/// The attributes each row's queue is CREATED with, for the row named by `name`.
fn created_with(name: &str) -> Value {
    match name {
        "surfaces-delayed" => json!({ "DelaySeconds": "3" }),
        "surfaces-configured" => json!({
            "VisibilityTimeout": "45",
            "MaximumMessageSize": "2048",
            "ReceiveMessageWaitTimeSeconds": "7",
        }),
        "surfaces-ordered.fifo" => json!({ "FifoQueue": "true", "ContentBasedDeduplication": "true" }),
        _ => Value::Null,
    }
}

#[tokio::test]
async fn every_configuration_item_read_reports_the_queue_the_recorder_discovered() {
    let emulator = Emulator::new();
    emulator.recording().await;

    for (case, name, attribute, updated) in QUEUES {
        let queue_url = create_queue(case, &emulator, name, &created_with(name)).await;
        let absent = never_created_queue_url(&format!("{name}-never-created"));

        // The resource's FIRST item is the one that discovered it, and its configuration
        // carries the live attributes the owning service reports.
        let items = await_history(case, &emulator, &queue_url, "a discovery item", |items| {
            !items.is_empty()
        })
        .await;
        assert_eq!(
            statuses(&items),
            vec![RESOURCE_DISCOVERED.to_string()],
            "case {case:?}: the only item of a newly discovered resource is its discovery, \
             got {items:?}"
        );
        let item = items[0].clone();
        assert_eq!(
            item["resourceId"],
            json!(queue_url),
            "case {case:?}: the item names the queue's own url as its resourceId, got {item}"
        );
        assert_eq!(
            item["resourceName"],
            json!(name),
            "case {case:?}: the item names the queue's custom name, got {item}"
        );
        assert_eq!(
            item["awsRegion"],
            json!(REGION),
            "case {case:?}: the item names the Region the resource was recorded in, got {item}"
        );
        let queue_arn = live_attribute(case, &emulator, &queue_url, "QueueArn").await;
        assert_eq!(
            item["arn"], queue_arn,
            "case {case:?}: the item carries the ARN Amazon SQS reports for the queue, got \
             {item}"
        );
        let live = live_attribute(case, &emulator, &queue_url, attribute).await;
        assert_eq!(
            configuration(case, &item)[attribute],
            live,
            "case {case:?}: the recorded {attribute} is the live one, got {item}"
        );

        // The batched read answers the key the inventory holds an item for and leaves the
        // key it holds none for out of BOTH of its lists: this read processed every key it
        // was handed, so none is left for a later request.
        let mixed = batch_get(
            &emulator,
            &[
                (QUEUE_RESOURCE_TYPE, queue_url.as_str()),
                (QUEUE_RESOURCE_TYPE, absent.as_str()),
            ],
        )
        .await;
        assert_eq!(
            batched_ids(case, &mixed),
            vec![queue_url.clone()],
            "case {case:?}: a mixed batch answers the recorded key only, got {mixed}"
        );
        assert_eq!(
            mixed["unprocessedResourceKeys"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            0,
            "case {case:?}: a key this read processed is not left unprocessed, got {mixed}"
        );

        // The query surface answers a row for the recorded queue, and the refusal names
        // the key the account recorded nothing for.
        let selected = emulator
            .config(
                "SelectResourceConfig",
                &json!({
                    "Expression": format!(
                        "SELECT resourceId, resourceType WHERE resourceType = \
                         '{QUEUE_RESOURCE_TYPE}' AND resourceId = '{queue_url}'"
                    ),
                }),
            )
            .await;
        let rows: Vec<Value> = selected["Results"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected Results, got {selected}"))
            .iter()
            .map(|row| {
                let text = row.as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: a result is a JSON document, got {selected}")
                });
                serde_json::from_str::<Value>(text).unwrap_or_else(|error| {
                    panic!("case {case:?}: a result parses as JSON, got {error} for {text:?}")
                })
            })
            .collect();
        assert_eq!(
            rows,
            vec![json!({ "resourceId": queue_url, "resourceType": QUEUE_RESOURCE_TYPE })],
            "case {case:?}: the query answers one row for the recorded queue, got {selected}"
        );

        let (status, refusal) = emulator
            .call(
                "config",
                "StarlingDoveService",
                "1.1",
                "GetResourceConfigHistory",
                &json!({ "resourceType": QUEUE_RESOURCE_TYPE, "resourceId": absent }),
            )
            .await;
        assert_eq!(
            (status, refusal["__type"].as_str()),
            (400, Some(NOT_DISCOVERED)),
            "case {case:?}: a key the account recorded nothing for is refused, got {status} \
             with {refusal}"
        );
        assert_eq!(
            refusal["message"].as_str(),
            Some(
                format!(
                    "Resource {absent} of resourceType:{QUEUE_RESOURCE_TYPE} is unknown or has \
                     not been discovered"
                )
                .as_str()
            ),
            "case {case:?}: the refusal names the key the request asked about, got {refusal}"
        );

        // A state the resource CHANGES into records one further item, which is an update
        // of an already discovered resource.
        emulator
            .sqs(
                "SetQueueAttributes",
                &json!({ "QueueUrl": queue_url, "Attributes": { attribute: updated } }),
            )
            .await;
        let changed = await_history(case, &emulator, &queue_url, "the changed state", |items| {
            items
                .first()
                .and_then(|item| recorded_value(item, attribute))
                .is_some_and(|recorded| recorded == updated)
        })
        .await;
        assert_eq!(
            configuration(case, &changed[0])[attribute],
            json!(updated),
            "case {case:?}: the newest item records the state the change left, got {:?}",
            changed[0]
        );
        assert_eq!(
            statuses(&changed).last().map(String::as_str),
            Some(RESOURCE_DISCOVERED),
            "case {case:?}: the oldest item of the resource stays its discovery, got {:?}",
            statuses(&changed)
        );

        // The resource going away in its own service records the FINAL item, and the
        // batched read stops reporting a resource whose deletion is recorded ("The API
        // does not return results for deleted resources").
        emulator
            .sqs("DeleteQueue", &json!({ "QueueUrl": queue_url }))
            .await;
        let deleted = await_history(case, &emulator, &queue_url, "the deletion item", |items| {
            statuses(items).first().map(String::as_str) == Some(RESOURCE_DELETED)
        })
        .await;
        assert!(
            deleted.len() > changed.len(),
            "case {case:?}: the deletion adds an item rather than replacing the ones before \
             it, got {:?}",
            statuses(&deleted)
        );
        let batched = batch_get(&emulator, &[(QUEUE_RESOURCE_TYPE, queue_url.as_str())]).await;
        assert_eq!(
            batched_ids(case, &batched),
            Vec::<String>::new(),
            "case {case:?}: a resource whose deletion is recorded has no current item to \
             report, got {batched}"
        );
    }
}

#[tokio::test]
async fn a_recorder_its_linked_service_turned_on_records_with_no_start_call() {
    // The rows differ along which linked service owns the recorder: the recorder it
    // creates is recording from the instant it was created, so the account's resources are
    // recorded for it without the start call a caller cannot make for one.
    for (index, service_principal) in SERVICE_LINKED_PRINCIPALS.into_iter().enumerate() {
        let case = service_principal;
        let emulator = Emulator::new();
        let created = emulator
            .config(
                "PutServiceLinkedConfigurationRecorder",
                &json!({ "ServicePrincipal": service_principal }),
            )
            .await;
        let recorder = created["Name"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected the assigned Name, got {created}"))
            .to_string();

        // The recorder reports that it is recording, and no start call was made for it.
        let described = emulator
            .config(
                "DescribeConfigurationRecorderStatus",
                &json!({ "ConfigurationRecorderNames": [recorder] }),
            )
            .await;
        assert_eq!(
            described["ConfigurationRecordersStatus"][0]["recording"],
            json!(true),
            "case {case:?}: a service-linked recorder is always recording, got {described}"
        );

        // So the queue the account holds is recorded for it, with the configuration the
        // owning service reports.
        let name = format!("linked-queue-{index}");
        let queue_url = create_queue(case, &emulator, &name, &json!({ "DelaySeconds": "5" })).await;
        let items = await_history(case, &emulator, &queue_url, "a discovery item", |items| {
            !items.is_empty()
        })
        .await;
        assert_eq!(
            statuses(&items),
            vec![RESOURCE_DISCOVERED.to_string()],
            "case {case:?}: the item is the queue's discovery, got {items:?}"
        );
        assert_eq!(
            configuration(case, &items[0])["DelaySeconds"],
            json!("5"),
            "case {case:?}: the item records the state the queue is in, got {:?}",
            items[0]
        );

        // And the batched read projects that same recorded item.
        let batched = batch_get(&emulator, &[(QUEUE_RESOURCE_TYPE, queue_url.as_str())]).await;
        assert_eq!(
            batched_ids(case, &batched),
            vec![queue_url.clone()],
            "case {case:?}: the batched read reports the recorded queue, got {batched}"
        );
    }
}
