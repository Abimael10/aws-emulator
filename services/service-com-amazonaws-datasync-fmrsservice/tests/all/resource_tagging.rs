// SPDX-License-Identifier: CC0-1.0

//! `TagResource`, `UntagResource` and `ListTagsForResource`, driven end to end over HTTP.
//!
//! RULE: a tag is key-value state held BY a resource, so `ListTagsForResource` reports
//! exactly the tags the resource its `ResourceArn` names holds at that moment — whatever
//! put them there, a create that carried `Tags` or a later `TagResource` — `TagResource`
//! applies tags by KEY (a key already held has its value replaced, never repeated),
//! `UntagResource` removes the keys it names and nothing else, and an ARN that resolves to
//! no resource this account holds is refused rather than answered — by this surface's one
//! generic not-found class and fixed wording, whatever kind of resource the ARN addresses.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: FmrsService.<Operation>` and an `awsJson1_1` body, dispatched by the
//! multi-service router. Nothing reaches into the state struct, so a response that is not
//! computed from persisted state cannot pass.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite signs for.
const SIGNED_REGION: &str = "us-west-2";

/// The class a refusal of the TAGGING surface naming a resource this account does not hold
/// carries, whatever kind of resource the ARN addresses.
const NOT_FOUND: &str = "NotFound";

/// The whole message that refusal is worded with: fixed text, with no variable slot — it
/// names neither the ARN the request carried nor the kind of resource it addresses.
const NOT_FOUND_MESSAGE: &str = "Not found";

/// The class an operation of the TASK surface — one whose own addressed resource is the
/// task — reports for a task this account does not hold, which is NOT the tagging surface's
/// generic class.
const SYNC_SET_NOT_FOUND: &str = "SyncSetNotFound";

/// The same, for an operation of the LOCATION surface.
const LOCATION_NOT_FOUND: &str = "LocationNotFound";

/// The most tags one resource holds, the ceiling a tagging call is refused past.
const MAX_TAGS_PER_RESOURCE: usize = 50;

/// How long a wait for a clock-owed transition is given before the test fails. Generous on
/// purpose: the test waits for the CONDITION (a task that has left `CREATING`), never for a
/// fixed time, so a loaded host only polls more often.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How long one poll of a clock-owed transition waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A router with just this service mounted on a fresh engine.
fn service() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_datasync_fmrsservice::EmulatedService::new(engine),
    );
    router
}

/// One `awsJson1_1` call, returning the HTTP status and the parsed response body.
async fn call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("FmrsService.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDDATASYNCTEST/20260101/{SIGNED_REGION}\
                 /datasync/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// A call that must succeed, returning its body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// A call that must be refused with HTTP 400, the one modelled client error, `message`, and
/// `class` in both code members (or neither member set, when the class has no discriminator).
async fn refused(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    class: Option<&str>,
    message: &str,
    case: &str,
) {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to answer 400, got {status} with {parsed}"
    );
    assert!(
        parsed["__type"]
            .as_str()
            .unwrap_or_default()
            .ends_with("InvalidRequestException"),
        "case {case:?}: expected an InvalidRequestException from {operation}, got {parsed}"
    );
    assert_eq!(
        parsed["message"].as_str(),
        Some(message),
        "case {case:?}: wrong refusal wording from {operation}, got {parsed}"
    );
    for member in ["errorCode", "datasyncErrorCode"] {
        assert_eq!(
            parsed[member].as_str(),
            class,
            "case {case:?}: expected {member} {class:?} from {operation}, got {parsed}"
        );
    }
}

/// A required string member of a response body.
fn string(body: &Value, member: &str, case: &str) -> String {
    body[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member}, got {body}"))
        .to_string()
}

/// A tag list as a request body carries it.
fn tag_list(tags: &[(&str, &str)]) -> Value {
    Value::Array(
        tags.iter()
            .map(|(key, value)| json!({ "Key": key, "Value": value }))
            .collect(),
    )
}

/// The `Tags` of one `ListTagsForResource` response, in the order it listed them.
fn listed_tags(page: &Value, case: &str) -> Vec<(String, String)> {
    page["Tags"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Tags array, got {page}"))
        .iter()
        .map(|tag| {
            (
                tag["Key"].as_str().unwrap_or_default().to_string(),
                tag["Value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Every tag the resource `arn` names holds, on one unpaginated call.
async fn tags_of(router: &MultiServiceRouter, arn: &str, case: &str) -> Vec<(String, String)> {
    let page = ok(
        router,
        "ListTagsForResource",
        json!({ "ResourceArn": arn }),
        case,
    )
    .await;
    assert!(
        page["NextToken"].is_null(),
        "case {case:?}: a page holding every tag must carry no NextToken, got {page}"
    );
    let listed = listed_tags(&page, case);
    let mut keys: Vec<&String> = listed.iter().map(|(key, _)| key).collect();
    let before = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(
        keys.len(),
        before,
        "case {case:?}: a key must be reported once, got {listed:?}"
    );
    listed
}

/// A well-formed ARN of one of this service's resources that this service never minted:
/// `resource` is the ARN's resource part.
fn never_minted(resource: &str) -> String {
    format!(
        "arn:aws:datasync:{SIGNED_REGION}:{account}:{resource}",
        account = engine::emulator_account_id()
    )
}

/// An Amazon S3 transfer location for `bucket`, created with `tags` and returned by ARN.
async fn given_s3_location(
    router: &MultiServiceRouter,
    bucket: &str,
    tags: Value,
    case: &str,
) -> String {
    let mut request = json!({
        "S3BucketArn": format!("arn:aws:s3:::{bucket}"),
        "S3Config": { "BucketAccessRoleArn": "arn:aws:iam::000000000000:role/datasync" },
    });
    if !tags.is_null() {
        request["Tags"] = tags;
    }
    let created = ok(router, "CreateLocationS3", request, case).await;
    string(&created, "LocationArn", case)
}

/// A transfer task between two fresh Amazon S3 locations, created with `tags`.
async fn given_task(router: &MultiServiceRouter, name: &str, tags: Value, case: &str) -> String {
    let source = given_s3_location(router, &format!("{name}-source"), Value::Null, case).await;
    let destination =
        given_s3_location(router, &format!("{name}-destination"), Value::Null, case).await;
    let mut request = json!({
        "SourceLocationArn": source,
        "DestinationLocationArn": destination,
        "Name": name,
    });
    if !tags.is_null() {
        request["Tags"] = tags;
    }
    let created = ok(router, "CreateTask", request, case).await;
    string(&created, "TaskArn", case)
}

/// An execution of a fresh task, started with `tags` and returned by ARN.
///
/// A task leaves `CREATING` on its own, and only an `AVAILABLE` task starts an execution, so
/// the start is retried until the task has settled — the CONDITION, not a fixed wait.
async fn given_task_execution(
    router: &MultiServiceRouter,
    name: &str,
    tags: Value,
    case: &str,
) -> String {
    let task = given_task(router, name, Value::Null, case).await;
    let mut request = json!({ "TaskArn": task });
    if !tags.is_null() {
        request["Tags"] = tags;
    }
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, parsed) = call(router, "StartTaskExecution", request.clone()).await;
        if status == 200 {
            return string(&parsed, "TaskExecutionArn", case);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the task never became startable, last answer {status} with {parsed}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// One taggable resource this suite mutates the tags of.
struct Fixture {
    case: &'static str,
    /// The tags the create carried, which the first read must report.
    created: Vec<(String, String)>,
    arn: String,
}

/// One resource of every kind this service holds tags on, created with a different tag list
/// each: the rows differ along the kind of resource, how many tags the create carried (none
/// at all, one, several) and which characters the keys and values use.
async fn fixtures(router: &MultiServiceRouter) -> Vec<Fixture> {
    // (case, the tags the create carries)
    let s3_tags: [(&str, &str); 2] = [("environment", "production"), ("owner", "data-platform")];
    let efs_tags: [(&str, &str); 1] = [("purpose", "backup")];
    // The key and value charsets the model admits (`TagKey` pattern
    // `^[a-zA-Z0-9\s+=._:/-]+$`, `TagValue` pattern `^[a-zA-Z0-9\s+=._:@/-]+$`): a value may
    // carry an `@`, a key may not, and both admit spaces, dots, colons and slashes.
    let object_storage_tags: [(&str, &str); 3] = [
        ("cost center", "cc.42"),
        ("path/segment:part", "a+b=c"),
        ("contact", "team@example.com"),
    ];
    let task_tags: [(&str, &str); 2] = [("tier", "gold"), ("owner", "data-platform")];
    let execution_tags: [(&str, &str); 1] = [("run", "nightly")];

    let s3 = given_s3_location(router, "tagging-s3", tag_list(&s3_tags), "an S3 location").await;
    let efs = ok(
        router,
        "CreateLocationEfs",
        json!({
            "EfsFilesystemArn":
                "arn:aws:elasticfilesystem:us-east-2:000000000000:file-system/fs-08e04cd4",
            "Ec2Config": {
                "SubnetArn": "arn:aws:ec2:us-east-2:000000000000:subnet/subnet-1234567890abcdef1",
                "SecurityGroupArns": [
                    "arn:aws:ec2:us-east-2:000000000000:security-group/sg-1234567890abcdef2"
                ],
            },
            "Tags": tag_list(&efs_tags),
        }),
        "an Amazon EFS location",
    )
    .await;
    let azure_blob = ok(
        router,
        "CreateLocationAzureBlob",
        json!({
            "ContainerUrl": "https://tagging.blob.core.windows.net/container",
            "AuthenticationType": "NONE",
        }),
        "an untagged Azure Blob Storage location",
    )
    .await;
    let object_storage = ok(
        router,
        "CreateLocationObjectStorage",
        json!({
            "ServerHostname": "storage.example.com",
            "BucketName": "objects",
            "Tags": tag_list(&object_storage_tags),
        }),
        "an object storage location",
    )
    .await;
    let task = given_task(router, "tagging-task", tag_list(&task_tags), "a task").await;
    let execution = given_task_execution(
        router,
        "tagging-execution",
        tag_list(&execution_tags),
        "a task execution",
    )
    .await;

    let owned = |tags: &[(&str, &str)]| -> Vec<(String, String)> {
        tags.iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    };
    vec![
        Fixture {
            case: "an S3 location",
            created: owned(&s3_tags),
            arn: s3,
        },
        Fixture {
            case: "an Amazon EFS location",
            created: owned(&efs_tags),
            arn: string(&efs, "LocationArn", "an Amazon EFS location"),
        },
        Fixture {
            case: "an untagged Azure Blob Storage location",
            created: Vec::new(),
            arn: string(
                &azure_blob,
                "LocationArn",
                "an untagged Azure Blob Storage location",
            ),
        },
        Fixture {
            case: "an object storage location",
            created: owned(&object_storage_tags),
            arn: string(&object_storage, "LocationArn", "an object storage location"),
        },
        Fixture {
            case: "a task",
            created: owned(&task_tags),
            arn: task,
        },
        Fixture {
            case: "a task execution",
            created: owned(&execution_tags),
            arn: execution,
        },
    ]
}

/// RULE: every taggable resource reports the tags it holds, and a tag mutation on one is
/// visible on the next read of that resource and on no other resource.
///
/// The rows span the kinds of resource this service holds tags on and the tag lists their
/// creates carried; each row is then mutated the three ways a tagging call can change a
/// list — a key added, a key already held given a new value, and a key removed.
#[tokio::test]
async fn tag_mutations_are_visible_on_every_taggable_resource() {
    let router = service();
    let fixtures = fixtures(&router).await;

    // What a create stored is what the first read reports.
    for fixture in &fixtures {
        let listed = tags_of(&router, &fixture.arn, fixture.case).await;
        assert_eq!(
            listed, fixture.created,
            "case {:?}: expected the tags the create carried, got {listed:?}",
            fixture.case
        );
    }

    for (index, fixture) in fixtures.iter().enumerate() {
        let case = fixture.case;

        // A key the resource does not hold is added; a key it does hold has its value
        // replaced where it already sits.
        let replaced = fixture.created.first().map(|(key, _)| key.clone());
        let mut applying: Vec<(&str, &str)> = vec![("stage", "canary")];
        if let Some(key) = &replaced {
            applying.push((key.as_str(), "replaced-value"));
        }
        ok(
            &router,
            "TagResource",
            json!({ "ResourceArn": &fixture.arn, "Tags": tag_list(&applying) }),
            case,
        )
        .await;

        let mut expected: Vec<(String, String)> = fixture.created.clone();
        if let Some(key) = &replaced {
            expected[0] = (key.clone(), "replaced-value".to_string());
        }
        expected.push(("stage".to_string(), "canary".to_string()));
        let listed = tags_of(&router, &fixture.arn, case).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected the applied tags, got {listed:?}"
        );

        // Removing a key the resource holds together with one it never held removes exactly
        // the first: the second asks for a state the resource is already in.
        let removed = expected.last().expect("a tag was just applied").0.clone();
        ok(
            &router,
            "UntagResource",
            json!({
                "ResourceArn": &fixture.arn,
                "Keys": [removed, "never-applied".to_string()],
            }),
            case,
        )
        .await;
        expected.retain(|(key, _)| *key != removed);
        let listed = tags_of(&router, &fixture.arn, case).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected the surviving tags, got {listed:?}"
        );

        // Tags are held BY a resource: mutating this one left every other resource's tags
        // where they were.
        for other in fixtures.iter().skip(index + 1) {
            let listed = tags_of(&router, &other.arn, other.case).await;
            assert_eq!(
                listed, other.created,
                "case {:?}: tagging {case:?} must not change another resource's tags, got {listed:?}",
                other.case
            );
        }
    }
}

/// RULE: `ListTagsForResource` pages over the resource's tags in the order they were
/// applied, resuming exactly where the previous page ended and omitting the token on the
/// last page; a `NextToken` this service never issued is refused.
///
/// The rows are every page size from 1 to one past the number of tags held, which exhausts
/// the sizes that can split them, plus the modelled maximum `MaxResults` admits.
#[tokio::test]
async fn list_tags_for_resource_pages_in_application_order() {
    let router = service();
    let arn = given_s3_location(&router, "tagging-paged", Value::Null, "paging").await;

    let applied: Vec<(String, String)> = (0..7)
        .map(|index| (format!("key-{index}"), format!("value-{index}")))
        .collect();
    let requested: Vec<(&str, &str)> = applied
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    ok(
        &router,
        "TagResource",
        json!({ "ResourceArn": &arn, "Tags": tag_list(&requested) }),
        "paging",
    )
    .await;

    for page_size in [1usize, 2, 3, 4, 7, 8, 100] {
        let case = format!("MaxResults={page_size}");
        let mut seen: Vec<(String, String)> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;

        loop {
            let mut request = json!({ "ResourceArn": &arn, "MaxResults": page_size });
            if let Some(token) = &token {
                request["NextToken"] = json!(token);
            }
            let page = ok(&router, "ListTagsForResource", request, &case).await;
            let tags = listed_tags(&page, &case);
            assert!(
                tags.len() <= page_size,
                "case {case:?}: a page carried {} tags, more than MaxResults",
                tags.len()
            );
            for tag in tags {
                assert!(
                    !seen.contains(&tag),
                    "case {case:?}: {tag:?} appeared on two pages"
                );
                seen.push(tag);
            }
            token = page["NextToken"].as_str().map(ToString::to_string);
            pages += 1;
            assert!(
                pages <= applied.len() + 2,
                "case {case:?}: pagination did not terminate after {pages} pages"
            );
            if token.is_none() {
                break;
            }
        }

        assert_eq!(
            seen, applied,
            "case {case:?}: paging must yield every tag in the order they were applied"
        );
        let expected_pages = applied.len().div_ceil(page_size);
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: expected {expected_pages} pages, walked {pages}"
        );
    }

    // A token this service never issued is refused rather than silently restarting.
    for bad in ["not-a-token", "ZZZZ", "00"] {
        refused(
            &router,
            "ListTagsForResource",
            json!({ "ResourceArn": &arn, "NextToken": bad }),
            None,
            "NextToken is not a token this service issued.",
            bad,
        )
        .await;
    }
}

/// Every tagging operation, with a body naming `arn` as its `ResourceArn`.
fn tagging_calls(arn: &str) -> [(&'static str, Value); 3] {
    [
        ("ListTagsForResource", json!({ "ResourceArn": arn })),
        (
            "TagResource",
            json!({ "ResourceArn": arn, "Tags": tag_list(&[("stage", "canary")]) }),
        ),
        (
            "UntagResource",
            json!({ "ResourceArn": arn, "Keys": ["stage"] }),
        ),
    ]
}

/// RULE: the tagging surface addresses every kind of resource through one `ResourceArn`, so
/// an ARN that resolves to no resource this account holds earns ONE generic refusal from all
/// three tagging operations — the same class in both code members and the same fixed
/// message — whatever kind of resource the ARN addresses.
///
/// The rows span the kinds the modelled `TaggableResourceArn` pattern admits within its
/// 128-character ceiling (a location, a task, an execution nested under its task, an agent
/// and a storage system — the last two kinds this service holds no record of at all), which
/// ids they name, and which account the ARN is of.
#[tokio::test]
async fn tagging_refuses_every_unheld_resource_kind_with_one_generic_not_found() {
    let router = service();

    // (case, the ARN the request carries)
    let rows: [(&str, String); 7] = [
        (
            "an unheld location",
            never_minted("location/loc-0abcdef1234567890"),
        ),
        (
            "another unheld location",
            never_minted("location/loc-1234567890abcdef0"),
        ),
        (
            "an unheld task",
            never_minted("task/task-0abcdef1234567890"),
        ),
        (
            "an unheld execution of an unheld task",
            never_minted("task/task-0abcdef1234567890/execution/exec-0123456789abcdef0"),
        ),
        (
            "an agent, a kind this service holds none of",
            never_minted("agent/agent-0abcdef1234567890"),
        ),
        (
            "a storage system, a kind this service holds none of",
            never_minted("system/storage-system-0123abcd-4567-89ef-0123-456789abcdef"),
        ),
        (
            "a task of another account",
            format!(
                "arn:aws:datasync:{SIGNED_REGION}:210987654321:task/task-0abcdef1234567890"
            ),
        ),
    ];

    for (case, arn) in &rows {
        for (operation, body) in tagging_calls(arn) {
            refused(
                &router,
                operation,
                body,
                Some(NOT_FOUND),
                NOT_FOUND_MESSAGE,
                case,
            )
            .await;
        }
    }
}

/// RULE: the same refusal reports STATE, not the shape of an ARN — a resource this service
/// minted is tagged and read, and the SAME ARN earns the generic not-found refusal from
/// every tagging operation once the resource is deleted, while an operation of the surface
/// that OWNS that kind of resource keeps reporting its own class for it.
///
/// The rows differ along the kind of resource deleted — a transfer location and a transfer
/// task — and so along which owning surface's class the contrast is drawn against.
#[tokio::test]
async fn tagging_refuses_a_deleted_resource_generically_whatever_its_own_surface_reports() {
    let router = service();

    let location = given_s3_location(
        &router,
        "tagging-deleted",
        tag_list(&[("environment", "staging")]),
        "a deleted location",
    )
    .await;
    let task = given_task(
        &router,
        "tagging-deleted-task",
        tag_list(&[("tier", "gold")]),
        "a deleted task",
    )
    .await;

    // One resource of a kind this service holds, with the surface that owns that kind.
    struct Deleted<'a> {
        case: &'a str,
        arn: &'a str,
        /// The one tag the create carried, which the first read must report.
        created: (&'a str, &'a str),
        /// The delete that removes it: its operation and the member naming the resource.
        deleting: (&'a str, &'a str),
        /// A read of the surface that owns the kind, the same way.
        reading: (&'a str, &'a str),
        /// The class and whole wording THAT surface reports once the resource is gone.
        own_class: &'a str,
        own_message: String,
    }

    let rows: [Deleted; 2] = [
        Deleted {
            case: "a deleted location",
            arn: &location,
            created: ("environment", "staging"),
            deleting: ("DeleteLocation", "LocationArn"),
            reading: ("DescribeLocationS3", "LocationArn"),
            own_class: LOCATION_NOT_FOUND,
            own_message: format!("Location {location} is not found."),
        },
        Deleted {
            case: "a deleted task",
            arn: &task,
            created: ("tier", "gold"),
            deleting: ("DeleteTask", "TaskArn"),
            reading: ("DescribeTask", "TaskArn"),
            own_class: SYNC_SET_NOT_FOUND,
            own_message: format!(
                "Task {} is not found.",
                task.rsplit('/').next().unwrap_or(task.as_str())
            ),
        },
    ];

    for row in &rows {
        let case = row.case;
        // While the resource is held, its tags read back and a tagging call is served.
        let listed = tags_of(&router, row.arn, case).await;
        assert_eq!(
            listed,
            vec![(row.created.0.to_string(), row.created.1.to_string())],
            "case {case:?}: expected the tag the create carried, got {listed:?}"
        );

        let (delete_operation, delete_member) = row.deleting;
        ok(
            &router,
            delete_operation,
            json!({ delete_member: row.arn }),
            case,
        )
        .await;

        // Deleting it is what makes the refusal due, and every tagging operation reports it
        // in the one generic class and wording.
        for (operation, body) in tagging_calls(row.arn) {
            refused(
                &router,
                operation,
                body,
                Some(NOT_FOUND),
                NOT_FOUND_MESSAGE,
                case,
            )
            .await;
        }

        // The surface that owns the kind of resource still reports its own class for the
        // same ARN: the generic class belongs to the tagging surface, not to the state.
        let (read_operation, read_member) = row.reading;
        refused(
            &router,
            read_operation,
            json!({ read_member: row.arn }),
            Some(row.own_class),
            &row.own_message,
            case,
        )
        .await;
    }
}

/// RULE: one resource's tag list is bounded, so a call that would take it past the ceiling
/// is refused whole — nothing is applied — while a call that only replaces values of keys
/// already held is served however full the list is.
#[tokio::test]
async fn tagging_is_refused_past_the_tag_ceiling_without_applying_anything() {
    let router = service();
    let arn = given_s3_location(&router, "tagging-bounded", Value::Null, "the ceiling").await;

    // The ceiling is reached in one call: `InputTagList` admits exactly this many entries.
    let full: Vec<(String, String)> = (0..MAX_TAGS_PER_RESOURCE)
        .map(|index| (format!("key-{index}"), format!("value-{index}")))
        .collect();
    let requested: Vec<(&str, &str)> = full
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    ok(
        &router,
        "TagResource",
        json!({ "ResourceArn": &arn, "Tags": tag_list(&requested) }),
        "the ceiling",
    )
    .await;
    let listed = tags_of(&router, &arn, "the ceiling").await;
    assert_eq!(
        listed, full,
        "case \"the ceiling\": expected a full tag list, got {} tags",
        listed.len()
    );

    let at_capacity = format!("This resource already holds the maximum of {MAX_TAGS_PER_RESOURCE} tags.");

    // (case, the tags the refused call carries)
    let refused_calls: [(&str, Vec<(&str, &str)>); 2] = [
        ("one key past the ceiling", vec![("stage", "canary")]),
        (
            "a replacement and a new key together",
            vec![("key-0", "overwritten"), ("stage", "canary")],
        ),
    ];
    for (case, tags) in &refused_calls {
        refused(
            &router,
            "TagResource",
            json!({ "ResourceArn": &arn, "Tags": tag_list(tags) }),
            None,
            &at_capacity,
            case,
        )
        .await;
        let listed = tags_of(&router, &arn, case).await;
        assert_eq!(
            listed, full,
            "case {case:?}: a refused call must apply nothing, got {listed:?}"
        );
    }

    // Replacing the value of a key already held needs no room, so it is served.
    ok(
        &router,
        "TagResource",
        json!({ "ResourceArn": &arn, "Tags": tag_list(&[("key-0", "replaced-value")]) }),
        "a replacement at the ceiling",
    )
    .await;
    let mut expected = full.clone();
    expected[0] = ("key-0".to_string(), "replaced-value".to_string());
    let listed = tags_of(&router, &arn, "a replacement at the ceiling").await;
    assert_eq!(
        listed, expected,
        "case \"a replacement at the ceiling\": expected the replaced value, got {listed:?}"
    );

    // Removing a key makes room for one more.
    ok(
        &router,
        "UntagResource",
        json!({ "ResourceArn": &arn, "Keys": ["key-0"] }),
        "room after a removal",
    )
    .await;
    ok(
        &router,
        "TagResource",
        json!({ "ResourceArn": &arn, "Tags": tag_list(&[("stage", "canary")]) }),
        "room after a removal",
    )
    .await;
    let listed = tags_of(&router, &arn, "room after a removal").await;
    expected.remove(0);
    expected.push(("stage".to_string(), "canary".to_string()));
    assert_eq!(
        listed, expected,
        "case \"room after a removal\": expected the tag applied in the freed slot, got {listed:?}"
    );
}
