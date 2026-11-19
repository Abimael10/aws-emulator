// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for AWS Deadline Cloud's resource-tagging surface:
//! `TagResource`, `ListTagsForResource` and `UntagResource`
//! (`/2023-10-12/tags/{resourceArn}`, by method).
//!
//! The rule under test: a tagging call addresses ONE stored resource by the full ARN of a
//! resource of this service's namespace, whose resource field spells the same nesting the
//! model's paths spell (`farm/<farmId>/queue/<queueId>/job/<jobId>` for the job at
//! `/2023-10-12/farms/{farmId}/queues/{queueId}/jobs/{jobId}`). A tag write stores the
//! pairs it carries on that resource and replaces the value of a key it already holds; the
//! tag read reports exactly the pairs the resource holds, including the ones its create
//! stored; the tag removal removes only the keys it names and is idempotent. A value that
//! names no taggable resource of this API is refused as invalid input, and an ARN of such a
//! resource that this emulator holds no record of is refused as not found, naming the level
//! of its path that is missing.
//!
//! The rows of each table differ along what that rule generalises over: the KIND of
//! resource the ARN names (every one of the six the model gives a `tags` member and this
//! emulator stores), the contents of the tag map (one pair, several, an empty value, a key
//! outside the ASCII letters), the ORDER of the writes (add, replace, remove, remove
//! again), and the COUNT of tags against the per-resource bound.
//!
//! The harness (`Emulator`) and the fleet and worker helpers are the ones the other suites
//! drive the service with, so every call goes through routing, deserialization, constraint
//! validation, the handler and serialization, exactly as the router does in production.

use serde_json::{json, Map, Value};

use crate::farm_queue_job::{absent_id, error_code, id_of, string_at, Emulator};
use crate::fleet::create_fleet;
use crate::worker::create_worker;

/// The Region every call of this harness is signed for, which is the Region the ARNs this
/// suite builds name.
const REGION: &str = "us-east-1";

/// The `arnNamespace` this service publishes (`smithy_query(query="service_info")`), which
/// is the vendor field of its own resource ARNs.
const ARN_NAMESPACE: &str = "deadline";

/// The ARN of the resource `resource` names, in the one account this emulator presents.
///
/// The account is the emulator's own rather than a literal, so the ARN a caller builds is
/// the ARN the emulator's resources carry.
fn deadline_arn(resource: &str) -> String {
    format!(
        "arn:aws:{ARN_NAMESPACE}:{REGION}:{}:{resource}",
        engine::emulator_account_id()
    )
}

/// Percent-encode one path segment the way an AWS SDK encodes a NON-greedy `@httpLabel`:
/// everything outside the RFC 3986 unreserved set, so the `:` and `/` of an ARN travel as
/// `%3A` and `%2F` and the whole ARN stays ONE segment.
fn encode_label(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The one path all three tagging operations are bound to
/// (`smithy_query(query="operation", operation="TagResource")`:
/// `/2023-10-12/tags/{resourceArn}`).
fn tags_path(resource_arn: &str) -> String {
    format!("/2023-10-12/tags/{}", encode_label(resource_arn))
}

/// `ListTagsForResource` for `resource_arn`.
async fn list_tags(emulator: &Emulator, resource_arn: &str) -> (u16, Value) {
    emulator.get(&tags_path(resource_arn)).await
}

/// `TagResource` writing `body` onto `resource_arn`.
async fn tag_resource(emulator: &Emulator, resource_arn: &str, body: Value) -> (u16, Value) {
    emulator
        .call("POST", &tags_path(resource_arn), Some(body), None)
        .await
}

/// `UntagResource` removing `keys` from `resource_arn`: `tagKeys` is an `@httpQuery` list,
/// so each key travels as its own `tagKeys` parameter.
async fn untag_resource(emulator: &Emulator, resource_arn: &str, keys: &[&str]) -> (u16, Value) {
    let query = keys
        .iter()
        .map(|key| format!("tagKeys={}", encode_label(key)))
        .collect::<Vec<_>>()
        .join("&");
    emulator
        .call(
            "DELETE",
            &format!("{}?{query}", tags_path(resource_arn)),
            None,
            None,
        )
        .await
}

/// The pairs as the JSON object a `tags` member carries.
fn tag_object(tags: &[(&str, &str)]) -> Value {
    let mut map = Map::new();
    for (key, value) in tags {
        map.insert((*key).to_string(), json!(value));
    }
    Value::Object(map)
}

/// The tags `ListTagsForResource` reports for `resource_arn`, failing the test if the read
/// did not succeed.
async fn tags_reported(emulator: &Emulator, case: &str, resource_arn: &str) -> Value {
    let (status, body) = list_tags(emulator, resource_arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListTagsForResource to answer 200, got {status} with {body}"
    );
    body.get("tags")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a tags member in {body}"))
}

/// One stored resource a tagging call can reach: what to call it in a failure message, and
/// the ARN that names it.
struct Taggable {
    case: &'static str,
    arn: String,
}

/// A `CreateBudget` body for a budget tracking `queue_id`, carrying every member the model
/// requires, plus `tags`.
fn budget_body(display_name: &str, queue_id: &str, tags: Value) -> Value {
    json!({
        "displayName": display_name,
        "usageTrackingResource": {"queueId": queue_id},
        "approximateDollarLimit": 100.0,
        "actions": [{"type": "STOP_SCHEDULING_AND_COMPLETE_TASKS", "thresholdPercentage": 80.0}],
        "schedule": {"fixed": {"startTime": "2026-01-01T00:00:00Z", "endTime": "2026-02-01T00:00:00Z"}},
        "tags": tags,
    })
}

/// Create a budget under `farm_id` tracking `queue_id` and return its id.
async fn create_budget(
    emulator: &Emulator,
    farm_id: &str,
    queue_id: &str,
    display_name: &str,
    tags: Value,
) -> String {
    let (status, body) = emulator
        .post(
            &format!("/2023-10-12/farms/{farm_id}/budgets"),
            budget_body(display_name, queue_id, tags),
        )
        .await;
    assert_eq!(
        status, 200,
        "CreateBudget {display_name}: expected 200, got {status} with {body}"
    );
    id_of(&body, "budgetId")
}

/// One resource of every kind this service stores that the model gives a `tags` member —
/// `CreateFarm`, `CreateQueue`, `CreateFleet`, `CreateJob`, `CreateWorker` and
/// `CreateBudget` each take one — created with no tags, and the ARN of each.
async fn one_of_every_kind(emulator: &Emulator) -> Vec<Taggable> {
    let farm_id = emulator.farm("tagging").await;
    let queue_id = emulator.queue(&farm_id, "tagged-queue").await;
    let fleet_id = create_fleet(emulator, &farm_id, "tagged-fleet").await;
    let job_id = emulator.job(&farm_id, &queue_id, "tagged-job", 50).await;
    let (status, created) = create_worker(emulator, &farm_id, &fleet_id, json!({}), None).await;
    assert_eq!(
        status, 200,
        "CreateWorker: expected 200, got {status} with {created}"
    );
    let worker_id = string_at(&created, "workerId");
    let budget_id = create_budget(
        emulator,
        &farm_id,
        &queue_id,
        "tagged-budget",
        json!({}),
    )
    .await;

    vec![
        Taggable {
            case: "a farm",
            arn: deadline_arn(&format!("farm/{farm_id}")),
        },
        Taggable {
            case: "a queue of that farm",
            arn: deadline_arn(&format!("farm/{farm_id}/queue/{queue_id}")),
        },
        Taggable {
            case: "a fleet of that farm",
            arn: deadline_arn(&format!("farm/{farm_id}/fleet/{fleet_id}")),
        },
        Taggable {
            case: "a job of that queue",
            arn: deadline_arn(&format!("farm/{farm_id}/queue/{queue_id}/job/{job_id}")),
        },
        Taggable {
            case: "a worker of that fleet",
            arn: deadline_arn(&format!("farm/{farm_id}/fleet/{fleet_id}/worker/{worker_id}")),
        },
        Taggable {
            case: "a budget of that farm",
            arn: deadline_arn(&format!("farm/{farm_id}/budget/{budget_id}")),
        },
    ]
}

/// The tag maps the round-trip table writes, one per resource kind, so the rows differ
/// along the map's contents as well as along the kind of resource it is written to: one
/// pair, several pairs, a pair whose value is the empty string the model allows, a key
/// outside the ASCII letters, and a map with nothing in it.
fn tag_maps() -> Vec<Value> {
    vec![
        tag_object(&[("team", "lighting")]),
        tag_object(&[("team", "lighting"), ("shot", "042"), ("stage", "final")]),
        // "Tag keys and values are both required, but tag values can be empty strings."
        // (`smithy_query(query="doc", shape="TagResourceRequest", member="tags")`).
        tag_object(&[("retired", "")]),
        tag_object(&[("cost:centre", "fx/éclairage")]),
        tag_object(&[("a", "1"), ("b", "2")]),
        tag_object(&[]),
    ]
}

#[tokio::test]
async fn tag_resource_then_list_tags_reports_the_tags_of_every_kind_of_resource() {
    let emulator = Emulator::new();
    let resources = one_of_every_kind(&emulator).await;
    let maps = tag_maps();

    // Before any tag write, every resource reports the tags it was created with: none.
    for resource in &resources {
        let reported = tags_reported(&emulator, resource.case, &resource.arn).await;
        assert_eq!(
            reported,
            json!({}),
            "case {:?}: expected an untagged resource to report no tags, got {reported}",
            resource.case
        );
    }

    for (resource, tags) in resources.iter().zip(maps.iter()) {
        let case = resource.case;
        let (status, body) =
            tag_resource(&emulator, &resource.arn, json!({"tags": tags})).await;
        assert_eq!(
            status, 204,
            "case {case:?}: expected TagResource to answer 204, got {status} with {body}"
        );
    }

    // Each resource reports exactly the pairs written to it, and tagging one resource
    // tagged no other — a queue's tags are not its farm's.
    for (resource, tags) in resources.iter().zip(maps.iter()) {
        let case = resource.case;
        let reported = tags_reported(&emulator, case, &resource.arn).await;
        assert_eq!(
            reported, *tags,
            "case {case:?}: expected the tags the write stored, got {reported}"
        );
    }
}

#[tokio::test]
async fn create_time_tags_are_the_tags_the_tag_read_reports() {
    let emulator = Emulator::new();
    let created = tag_object(&[("owner", "pipeline"), ("phase", "")]);

    // The rule holds for the tags a CREATE stored, not only for the ones a tag write
    // stored: the rows are every kind of resource whose create takes a `tags` map.
    let farm_id = {
        let (status, body) = emulator
            .post(
                "/2023-10-12/farms",
                json!({"displayName": "created-tagged", "tags": created.clone()}),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateFarm: expected 200, got {status} with {body}"
        );
        id_of(&body, "farmId")
    };
    let queue_id = {
        let (status, body) = emulator
            .post(
                &format!("/2023-10-12/farms/{farm_id}/queues"),
                json!({"displayName": "created-tagged", "tags": created.clone()}),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateQueue: expected 200, got {status} with {body}"
        );
        id_of(&body, "queueId")
    };
    let fleet_id = {
        let (status, body) = emulator
            .post(
                &format!("/2023-10-12/farms/{farm_id}/fleets"),
                json!({
                    "displayName": "created-tagged",
                    "roleArn": crate::fleet::ROLE_ARN,
                    "maxWorkerCount": 4,
                    "configuration": crate::fleet::customer_managed("LINUX", "x86_64", json!({})),
                    "tags": created.clone(),
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateFleet: expected 200, got {status} with {body}"
        );
        id_of(&body, "fleetId")
    };
    let job_id = {
        let (status, body) = emulator
            .post(
                &format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}/jobs"),
                json!({
                    "template": json!({
                        "specificationVersion": "jobtemplate-2023-09",
                        "name": "created-tagged",
                        "steps": [],
                    })
                    .to_string(),
                    "templateType": "JSON",
                    "priority": 50,
                    "tags": created.clone(),
                }),
            )
            .await;
        assert_eq!(
            status, 201,
            "CreateJob: expected 201, got {status} with {body}"
        );
        id_of(&body, "jobId")
    };
    let worker_id = {
        let (status, body) = create_worker(
            &emulator,
            &farm_id,
            &fleet_id,
            json!({"tags": created.clone()}),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "CreateWorker: expected 200, got {status} with {body}"
        );
        string_at(&body, "workerId")
    };
    let budget_id = create_budget(
        &emulator,
        &farm_id,
        &queue_id,
        "created-tagged",
        created.clone(),
    )
    .await;

    let rows = vec![
        ("a farm created with tags", format!("farm/{farm_id}")),
        (
            "a queue created with tags",
            format!("farm/{farm_id}/queue/{queue_id}"),
        ),
        (
            "a fleet created with tags",
            format!("farm/{farm_id}/fleet/{fleet_id}"),
        ),
        (
            "a job created with tags",
            format!("farm/{farm_id}/queue/{queue_id}/job/{job_id}"),
        ),
        (
            "a worker created with tags",
            format!("farm/{farm_id}/fleet/{fleet_id}/worker/{worker_id}"),
        ),
        (
            "a budget created with tags",
            format!("farm/{farm_id}/budget/{budget_id}"),
        ),
    ];

    for (case, resource) in rows {
        let reported = tags_reported(&emulator, case, &deadline_arn(&resource)).await;
        assert_eq!(
            reported, created,
            "case {case:?}: expected the tags the create stored, got {reported}"
        );
    }
}

/// One step of the write table below: what to call, and the tags the resource holds
/// afterwards.
struct Write {
    /// What to call this step in a failure message.
    case: &'static str,
    /// The call to make.
    step: Step,
    /// The tags the resource holds once the call has been made.
    left: Value,
}

/// The call one [`Write`] makes.
enum Step {
    /// `TagResource` carrying this `tags` map.
    Tag(Value),
    /// `TagResource` carrying no `tags` member at all, which the model allows.
    TagNothing,
    /// `UntagResource` naming these keys.
    Untag(Vec<&'static str>),
}

#[tokio::test]
async fn tag_writes_add_and_replace_and_untag_removes_only_the_keys_it_names() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("tag-writes").await;
    let arn = deadline_arn(&format!("farm/{farm_id}"));

    // The rule: a tag write adds the pairs it carries and replaces the value of a key the
    // resource already holds, a removal takes away only the keys it names, and both are
    // idempotent. The rows walk one resource through those forms in order.
    let steps = vec![
        Write {
            case: "the first pair",
            step: Step::Tag(tag_object(&[("team", "lighting")])),
            left: tag_object(&[("team", "lighting")]),
        },
        Write {
            case: "a second key, leaving the first alone",
            step: Step::Tag(tag_object(&[("shot", "042")])),
            left: tag_object(&[("team", "lighting"), ("shot", "042")]),
        },
        Write {
            case: "the same key again, replacing its value",
            step: Step::Tag(tag_object(&[("team", "fx")])),
            left: tag_object(&[("team", "fx"), ("shot", "042")]),
        },
        Write {
            case: "the same pair twice, which changes nothing",
            step: Step::Tag(tag_object(&[("team", "fx")])),
            left: tag_object(&[("team", "fx"), ("shot", "042")]),
        },
        Write {
            case: "a write carrying no tags member",
            step: Step::TagNothing,
            left: tag_object(&[("team", "fx"), ("shot", "042")]),
        },
        Write {
            case: "removing one key",
            step: Step::Untag(vec!["shot"]),
            left: tag_object(&[("team", "fx")]),
        },
        Write {
            case: "removing that key again",
            step: Step::Untag(vec!["shot"]),
            left: tag_object(&[("team", "fx")]),
        },
        Write {
            case: "removing a key the resource never held",
            step: Step::Untag(vec!["never-written"]),
            left: tag_object(&[("team", "fx")]),
        },
        Write {
            case: "removing the last key",
            step: Step::Untag(vec!["team"]),
            left: tag_object(&[]),
        },
    ];

    for Write { case, step, left } in steps {
        let (status, body) = match &step {
            Step::Tag(tags) => tag_resource(&emulator, &arn, json!({"tags": tags})).await,
            Step::TagNothing => tag_resource(&emulator, &arn, json!({})).await,
            Step::Untag(keys) => untag_resource(&emulator, &arn, keys).await,
        };
        assert_eq!(
            status, 204,
            "case {case:?}: expected the tag write to answer 204, got {status} with {body}"
        );
        let reported = tags_reported(&emulator, case, &arn).await;
        assert_eq!(
            reported, left,
            "case {case:?}: expected the tags the write left, got {reported}"
        );
    }
}

/// The refusal one row of the table below expects: the status, the wire error code, and —
/// for a missing resource — the type and id the 404 reports.
struct Refusal {
    status: u16,
    code: &'static str,
    resource: Option<(&'static str, String)>,
}

#[tokio::test]
async fn an_arn_that_names_no_taggable_resource_is_refused_by_every_tagging_call() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("refusals").await;
    let queue_id = emulator.queue(&farm_id, "refusals").await;
    let fleet_id = create_fleet(&emulator, &farm_id, "refusals").await;
    let absent_farm = absent_id("farm", &"a".repeat(32));
    let absent_queue = absent_id("queue", &"b".repeat(32));
    let absent_job = absent_id("job", &"c".repeat(32));
    let absent_worker = absent_id("worker", &"d".repeat(32));
    let absent_budget = absent_id("budget", &"e".repeat(32));
    let absent_monitor = absent_id("monitor", &"f".repeat(32));
    let absent_limit = absent_id("limit", &"0".repeat(32));

    // The rule: a value that cannot address a taggable resource of this API is invalid
    // input (400), and an ARN that does address one this emulator holds no record of is a
    // missing resource (404) naming the level of its path that is missing. The rows differ
    // along WHY the value reaches nothing.
    let rows = vec![
        (
            "a value that is not an ARN",
            absent_farm.clone(),
            Refusal {
                status: 400,
                code: "ValidationException",
                resource: None,
            },
        ),
        (
            "an ARN of another vendor",
            "arn:aws:s3:::a-bucket".to_string(),
            Refusal {
                status: 400,
                code: "ValidationException",
                resource: None,
            },
        ),
        (
            "an ARN naming no resource",
            format!("arn:aws:{ARN_NAMESPACE}:{REGION}:000000000000:"),
            Refusal {
                status: 400,
                code: "ValidationException",
                resource: None,
            },
        ),
        (
            "a resource field naming a type with no id",
            deadline_arn("farm"),
            Refusal {
                status: 400,
                code: "ValidationException",
                resource: None,
            },
        ),
        (
            "a resource type this API gives no tags",
            deadline_arn(&format!("farm/{farm_id}/limit/{absent_limit}")),
            Refusal {
                status: 400,
                code: "ValidationException",
                resource: None,
            },
        ),
        (
            "a nesting the model's paths do not have",
            deadline_arn(&format!("farm/{farm_id}/fleet/{fleet_id}/job/{absent_job}")),
            Refusal {
                status: 400,
                code: "ValidationException",
                resource: None,
            },
        ),
        (
            "a farm this emulator does not hold",
            deadline_arn(&format!("farm/{absent_farm}")),
            Refusal {
                status: 404,
                code: "ResourceNotFoundException",
                resource: Some(("farm", absent_farm.clone())),
            },
        ),
        (
            "a queue of a held farm that does not exist",
            deadline_arn(&format!("farm/{farm_id}/queue/{absent_queue}")),
            Refusal {
                status: 404,
                code: "ResourceNotFoundException",
                resource: Some(("queue", absent_queue.clone())),
            },
        ),
        (
            "a job under a held queue that does not exist",
            deadline_arn(&format!("farm/{farm_id}/queue/{queue_id}/job/{absent_job}")),
            Refusal {
                status: 404,
                code: "ResourceNotFoundException",
                resource: Some(("job", absent_job.clone())),
            },
        ),
        (
            "a worker of a held fleet that does not exist",
            deadline_arn(&format!(
                "farm/{farm_id}/fleet/{fleet_id}/worker/{absent_worker}"
            )),
            Refusal {
                status: 404,
                code: "ResourceNotFoundException",
                resource: Some(("worker", absent_worker.clone())),
            },
        ),
        (
            "a budget of a held farm that does not exist",
            deadline_arn(&format!("farm/{farm_id}/budget/{absent_budget}")),
            Refusal {
                status: 404,
                code: "ResourceNotFoundException",
                resource: Some(("budget", absent_budget.clone())),
            },
        ),
        (
            "a queue of a farm this emulator does not hold, which names the farm",
            deadline_arn(&format!("farm/{absent_farm}/queue/{queue_id}")),
            Refusal {
                status: 404,
                code: "ResourceNotFoundException",
                resource: Some(("farm", absent_farm.clone())),
            },
        ),
        (
            "a monitor, which is taggable but which nothing has created",
            deadline_arn(&format!("monitor/{absent_monitor}")),
            Refusal {
                status: 404,
                code: "ResourceNotFoundException",
                resource: Some(("monitor", absent_monitor.clone())),
            },
        ),
    ];

    for (case, arn, expected) in rows {
        // Every one of the three calls reads the ARN the same way, so each is refused the
        // same way.
        let calls = vec![
            (
                "TagResource",
                tag_resource(&emulator, &arn, json!({"tags": {"team": "lighting"}})).await,
            ),
            ("ListTagsForResource", list_tags(&emulator, &arn).await),
            (
                "UntagResource",
                untag_resource(&emulator, &arn, &["team"]).await,
            ),
        ];
        for (operation, (status, body)) in calls {
            assert_eq!(
                status, expected.status,
                "case {case:?} on {operation}: expected {}, got {status} with {body}",
                expected.status
            );
            assert_eq!(
                error_code(&body),
                expected.code,
                "case {case:?} on {operation}: expected the {} code, got {body}",
                expected.code
            );
            if let Some((resource_type, resource_id)) = &expected.resource {
                assert_eq!(
                    body.get("resourceType").and_then(Value::as_str),
                    Some(*resource_type),
                    "case {case:?} on {operation}: expected the missing resource's type, got {body}"
                );
                assert_eq!(
                    body.get("resourceId").and_then(Value::as_str),
                    Some(resource_id.as_str()),
                    "case {case:?} on {operation}: expected the missing resource's id, got {body}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_tag_write_past_the_per_resource_bound_is_refused_and_stores_nothing() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("tag-bound").await;
    let arn = deadline_arn(&format!("farm/{farm_id}"));
    // The bound is read from the one place that states it, so the table cannot drift from
    // the figure the handler enforces.
    let bound = service_com_amazonaws_deadline_deadline::state::MAX_TAGS_PER_RESOURCE;

    // The resource is filled to the bound in one write, which must succeed: the bound
    // itself is allowed.
    let keys: Vec<String> = (0..bound).map(|index| format!("key-{index}")).collect();
    let mut at_the_bound = Map::new();
    for key in &keys {
        at_the_bound.insert(key.clone(), json!("v"));
    }
    let (status, body) = tag_resource(
        &emulator,
        &arn,
        json!({"tags": Value::Object(at_the_bound.clone())}),
    )
    .await;
    assert_eq!(
        status, 204,
        "case \"at the bound\": expected TagResource to answer 204, got {status} with {body}"
    );
    let reported = tags_reported(&emulator, "at the bound", &arn).await;
    assert_eq!(
        reported,
        Value::Object(at_the_bound.clone()),
        "case \"at the bound\": expected the {bound} tags the write stored, got {reported}"
    );

    // A key the resource already holds does not grow it, so a write of a stored key is
    // still accepted at the bound.
    let (status, body) = tag_resource(
        &emulator,
        &arn,
        json!({"tags": {keys[0].clone(): "replaced"}}),
    )
    .await;
    assert_eq!(
        status, 204,
        "case \"replacing a stored key at the bound\": expected 204, got {status} with {body}"
    );

    // One key past the bound is refused, and the refused write stores nothing.
    let (status, body) = tag_resource(&emulator, &arn, json!({"tags": {"one-too-many": "v"}})).await;
    assert_eq!(
        status, 400,
        "case \"past the bound\": expected 400, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case \"past the bound\": expected a ValidationException, got {body}"
    );
    let reported = tags_reported(&emulator, "past the bound", &arn).await;
    let after = reported
        .as_object()
        .unwrap_or_else(|| panic!("case \"past the bound\": expected a tag map, got {reported}"));
    assert_eq!(
        after.len(),
        bound,
        "case \"past the bound\": expected the refused write to store nothing, got {reported}"
    );
    assert_eq!(
        after.get(&keys[0]).and_then(Value::as_str),
        Some("replaced"),
        "case \"past the bound\": expected the replaced value to survive, got {reported}"
    );
}

#[tokio::test]
async fn a_tag_with_no_key_is_refused_and_stores_nothing() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("tag-keys").await;
    let arn = deadline_arn(&format!("farm/{farm_id}"));

    // "Tag keys and values are both required, but tag values can be empty strings."
    // (`smithy_query(query="doc", shape="TagResourceRequest", member="tags")`): the rows
    // are a map whose only key is empty, and a map pairing a good key with an empty one.
    let rows = vec![
        ("an empty key on its own", json!({"": "lighting"})),
        (
            "an empty key beside a key that is fine",
            json!({"team": "lighting", "": "x"}),
        ),
    ];

    for (case, tags) in rows {
        let (status, body) = tag_resource(&emulator, &arn, json!({"tags": tags})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected TagResource to answer 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {body}"
        );
        let reported = tags_reported(&emulator, case, &arn).await;
        assert_eq!(
            reported,
            json!({}),
            "case {case:?}: expected the refused write to store nothing, got {reported}"
        );
    }
}

#[tokio::test]
async fn a_tag_key_or_value_past_its_length_bound_is_refused_and_stores_nothing() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("tag-lengths").await;
    let arn = deadline_arn(&format!("farm/{farm_id}"));
    let key_bound = service_com_amazonaws_deadline_deadline::state::MAX_TAG_KEY_LENGTH;
    let value_bound = service_com_amazonaws_deadline_deadline::state::MAX_TAG_VALUE_LENGTH;

    // The rule: a tag key and a tag value each have a length bound, the bound itself is
    // accepted and one character past it is refused. The rows are each bound and each
    // bound plus one, counted in characters rather than bytes — so one row is made of
    // multi-byte characters.
    let at_the_key_bound = "k".repeat(key_bound);
    let past_the_key_bound = "k".repeat(key_bound + 1);
    let at_the_value_bound = "é".repeat(value_bound);
    let past_the_value_bound = "v".repeat(value_bound + 1);
    let rows = vec![
        (
            "a key at its length bound",
            json!({at_the_key_bound.clone(): "v"}),
            204,
        ),
        (
            "a key one character past its bound",
            json!({past_the_key_bound.clone(): "v"}),
            400,
        ),
        (
            "a value at its length bound, in multi-byte characters",
            json!({"long-value": at_the_value_bound.clone()}),
            204,
        ),
        (
            "a value one character past its bound",
            json!({"long-value": past_the_value_bound.clone()}),
            400,
        ),
    ];

    for (case, tags, expected) in rows {
        let before = tags_reported(&emulator, case, &arn).await;
        let (status, body) = tag_resource(&emulator, &arn, json!({"tags": tags})).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected TagResource to answer {expected}, got {status} with {body}"
        );
        let after = tags_reported(&emulator, case, &arn).await;
        if expected == 400 {
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?}: expected a ValidationException, got {body}"
            );
            assert_eq!(
                after, before,
                "case {case:?}: expected the refused write to store nothing, got {after}"
            );
        } else {
            assert_ne!(
                after, before,
                "case {case:?}: expected the accepted write to store its pair, got {after}"
            );
        }
    }

    // What the accepted rows left is exactly the two pairs at the bounds.
    let reported = tags_reported(&emulator, "the accepted pairs", &arn).await;
    assert_eq!(
        reported,
        json!({at_the_key_bound: "v", "long-value": at_the_value_bound}),
        "case \"the accepted pairs\": expected the two pairs at the bounds, got {reported}"
    );
}

#[tokio::test]
async fn the_tags_of_a_deleted_resource_are_gone_with_it() {
    let emulator = Emulator::new();
    let farm_id = emulator.farm("deleted").await;
    let queue_id = emulator.queue(&farm_id, "deleted").await;
    let arn = deadline_arn(&format!("farm/{farm_id}/queue/{queue_id}"));

    let (status, body) = tag_resource(
        &emulator,
        &arn,
        json!({"tags": {"team": "lighting"}}),
    )
    .await;
    assert_eq!(
        status, 204,
        "case \"tagging the queue\": expected 204, got {status} with {body}"
    );

    let (status, body) = emulator
        .call(
            "DELETE",
            &format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}"),
            None,
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"deleting the queue\": expected 200, got {status} with {body}"
    );

    // The resource is gone, so its tags are not reported, written or removed: every
    // tagging call is told the queue is missing.
    let (status, body) = list_tags(&emulator, &arn).await;
    assert_eq!(
        status, 404,
        "case \"reading a deleted queue's tags\": expected 404, got {status} with {body}"
    );
    assert_eq!(
        body.get("resourceId").and_then(Value::as_str),
        Some(queue_id.as_str()),
        "case \"reading a deleted queue's tags\": expected the queue's id, got {body}"
    );
}
