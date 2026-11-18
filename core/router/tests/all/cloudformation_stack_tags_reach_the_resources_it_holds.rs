// SPDX-License-Identifier: CC0-1.0

//! What a change to a stack's OWN tag set does to the resources that stack manages.
//!
//! The rule under test: **a stack's tags are the tags of the resources it manages
//! ("CloudFormation also propagates these tags to resources in the stack"), so a deployment
//! that puts a DIFFERENT tag set in force changes every resource the stack holds whose type an
//! owning service tags — each of them carries the new set and is UPDATED by that operation,
//! reaching `UPDATE_COMPLETE` while keeping the physical resource it already had — even where
//! its declaration is untouched. A resource of a type nothing tags, and a resource the
//! deployment does not act on, report no update.**
//!
//! Two crates have to agree for that to hold (CloudFormation deploys the tag set and records
//! what became of each resource, Amazon SQS owns the queues and is the only side that can say
//! which tags they really carry), so the suite lives here.
//!
//! The rows differ along the shape of the tag change the rule generalises over: a first tag on
//! a stack that had none, one tag added beside another, another value for a tag already in
//! force, and one of two tags taken away — plus the converse row, a deployment that changes one
//! resource's own declaration and the tag set of nothing, which must leave every resource it
//! does not act on exactly as it was.
//!
//! The last test is the `cdk import` case this suite's issue was filed about: nothing created
//! the queue a stack ADOPTS, so the tag set CloudFormation gives a resource it manages reaches
//! that queue for the first time as the import brings it under management — which is the
//! resource's own update, so it settles at `UPDATE_COMPLETE` under the identifier it was
//! adopted with, without being re-created, while the stack's operation stays the import it is.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery wire readers this suite shares with every other CloudFormation suite here.
use crate::query_wire::{elements, form_encode};

/// The region every request here is signed for, and so the region the stacks and queues belong
/// to.
const REGION: &str = "us-east-1";

/// The account id every queue url this emulator mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// How long a wait for a stack's clock-owed transitions may take before the test fails rather
/// than hangs: far above the span itself, so neither a loaded nor a fast host decides the
/// outcome. Nothing here asserts how long anything took.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long a wait sleeps between two looks at the stack it is waiting on.
const LOOK: Duration = Duration::from_millis(20);

/// The logical id of the queue every row's assertions are about.
const TAGGED: &str = "Tagged";

/// The logical id of the SECOND queue every stack holds: a resource no row's update acts on,
/// which a tag change nonetheless re-tags.
const OTHER: &str = "Other";

/// The logical id of the resource no service here tags — the type the emulator leaves
/// declared-only, so a tag change has nothing to apply to it.
const HANDLE: &str = "Handle";

/// The tag the TEMPLATE declares on the queue it creates, as distinct from the stack's own:
/// a resource keeps the tags its declaration asks for whatever the stack's tags become.
const DECLARED_TAG: (&str, &str) = ("Declared", "on-the-resource");

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call that must be accepted.
async fn cloudformation_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(String, String)],
) -> String {
    let (status, body) = cloudformation(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with {body}"
    );
    body
}

/// Request members as the wire reader above takes them, from borrowed pairs.
fn members(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// The `Tags.member.N.Key`/`Value` members one tag set is submitted as.
fn tag_members(tags: &[(&str, &str)]) -> Vec<(String, String)> {
    tags.iter()
        .enumerate()
        .flat_map(|(index, (key, value))| {
            let position = index + 1;
            [
                (
                    format!("Tags.member.{position}.Key"),
                    (*key).to_string(),
                ),
                (
                    format!("Tags.member.{position}.Value"),
                    (*value).to_string(),
                ),
            ]
        })
        .collect()
}

/// The text of the FIRST `<tag>` of an XML answer.
fn first_element(xml: &str, tag: &str) -> Option<String> {
    elements(xml, tag).into_iter().next()
}

/// Call one Amazon SQS operation over its awsJson1_0 wire.
async fn sqs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One SQS call that must succeed.
async fn sqs_ok(router: &MultiServiceRouter, case: &str, operation: &str, request: Value) -> Value {
    let (status, parsed) = sqs(router, operation, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// The tags Amazon SQS really holds for the queue at `url` — the only side that can say what a
/// queue carries.
async fn queue_tags(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
) -> BTreeMap<String, String> {
    let listed = sqs_ok(router, case, "ListQueueTags", json!({"QueueUrl": url})).await;
    listed
        .get("Tags")
        .and_then(Value::as_object)
        .map(|tags| {
            tags.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One attribute Amazon SQS reports for the queue at `url`.
async fn queue_attribute(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    name: &str,
) -> Option<String> {
    let reported = sqs_ok(
        router,
        case,
        "GetQueueAttributes",
        json!({"QueueUrl": url, "AttributeNames": ["All"]}),
    )
    .await;
    reported
        .get("Attributes")
        .and_then(Value::as_object)
        .and_then(|attributes| attributes.get(name))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The queue url `CreateQueue` mints for `name`, which is this type's physical resource id
/// ("Ref returns the queue URL").
fn queue_url(name: &str) -> String {
    format!("https://sqs.{REGION}.amazonaws.com/{ACCOUNT_ID}/{name}")
}

/// Whether `status` is one a stack operation has FINISHED in, so the records it left are the
/// ones to read.
fn is_settled(status: &str) -> bool {
    !status.is_empty() && !status.ends_with("_IN_PROGRESS")
}

/// Look at the stack again, every [`LOOK`], until the status it reports is one its last
/// operation settled in or [`SETTLE_DEADLINE`] passes — the transitions fall due on the
/// emulator's own clock, so the wait is on that CONDITION and never on a fixed time.
async fn settled_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut last = String::new();
    while Instant::now() < deadline {
        let body =
            cloudformation_ok(router, case, "DescribeStacks", &members(&[("StackName", stack)]))
                .await;
        last = first_element(&body, "StackStatus").unwrap_or_default();
        if is_settled(&last) {
            return last;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: {stack} never settled within {SETTLE_DEADLINE:?}, last was {last:?}")
}

/// `(the resource's status, its physical id)` as the stack reports them for `logical_id`.
async fn reported_resource(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> (String, String) {
    let body = cloudformation_ok(
        router,
        case,
        "DescribeStackResource",
        &members(&[("StackName", stack), ("LogicalResourceId", logical_id)]),
    )
    .await;
    (
        first_element(&body, "ResourceStatus").unwrap_or_default(),
        first_element(&body, "PhysicalResourceId").unwrap_or_default(),
    )
}

/// The template every row of the table deploys: two queues a service TAGS and one resource
/// nothing here tags, with `timeout` as the declared visibility timeout of the first queue —
/// the one property a row can change without touching the stack's tags.
fn template(stack: &str, timeout: u32) -> String {
    let (key, value) = DECLARED_TAG;
    format!(
        r#"{{"Resources":{{
            "{TAGGED}":{{"Type":"AWS::SQS::Queue","Properties":{{
                "QueueName":"{stack}-tagged","VisibilityTimeout":{timeout},
                "Tags":[{{"Key":"{key}","Value":"{value}"}}]}}}},
            "{OTHER}":{{"Type":"AWS::SQS::Queue","Properties":{{
                "QueueName":"{stack}-other"}}}},
            "{HANDLE}":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}}}}"#
    )
}

/// The tag set a queue of this stack is expected to carry: the stack's own tags beside the ones
/// the declaration asks for. CloudFormation's three `aws:cloudformation:*` records are not
/// among them — `CreateQueue` and `TagQueue` refuse a key in that reserved namespace to a
/// caller, so a queue carries the stack's tags without them.
fn expected_tags(stack_tags: &[(&str, &str)], declared: bool) -> BTreeMap<String, String> {
    let mut expected: BTreeMap<String, String> = stack_tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    if declared {
        expected.insert(DECLARED_TAG.0.to_string(), DECLARED_TAG.1.to_string());
    }
    expected
}

/// One row of the table: the tag set the stack is created with, the deployment that follows, and
/// what each resource must report once it has settled.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// The stack-level tags in force when the stack is created.
    created_with: &'static [(&'static str, &'static str)],
    /// The stack-level tags the update submits.
    submitted: &'static [(&'static str, &'static str)],
    /// The declared visibility timeout the update submits for the first queue: the same value
    /// the create used in every row but the converse one, which changes that declaration and
    /// no tag.
    timeout: u32,
    /// Whether the update is a change to the TAG SET in force, and so to every resource the
    /// stack holds that a service tags.
    retags: bool,
}

/// The visibility timeout every stack is created with.
const CREATED_TIMEOUT: u32 = 30;

/// RULE: a deployment that puts a different stack-level tag set in force propagates it to every
/// resource the stack manages whose type a service tags — each of them carries the new set and
/// is UPDATED by that operation, keeping the physical resource it already had — while a resource
/// of a type nothing tags, and one the deployment does not act on, report no update.
#[tokio::test]
async fn a_changed_stack_tag_set_updates_every_resource_a_service_tags() {
    let router = build_router();
    let rows: &[Row] = &[
        Row {
            case: "a first tag on a stack that had none",
            stack_name: "tags-first",
            created_with: &[],
            submitted: &[("Owner", "platform")],
            timeout: CREATED_TIMEOUT,
            retags: true,
        },
        Row {
            case: "a second tag beside the one it had",
            stack_name: "tags-added",
            created_with: &[("Owner", "platform")],
            submitted: &[("Owner", "platform"), ("Tier", "gold")],
            timeout: CREATED_TIMEOUT,
            retags: true,
        },
        Row {
            case: "another value for a tag already in force",
            stack_name: "tags-revalued",
            created_with: &[("Owner", "platform")],
            submitted: &[("Owner", "payments")],
            timeout: CREATED_TIMEOUT,
            retags: true,
        },
        Row {
            case: "one of the two tags it had taken away",
            stack_name: "tags-dropped",
            created_with: &[("Owner", "platform"), ("Tier", "gold")],
            submitted: &[("Owner", "platform")],
            timeout: CREATED_TIMEOUT,
            retags: true,
        },
        // The converse: a deployment that changes ONE resource's own declaration and no tag at
        // all. The queue it declares differently is updated, and the resource it does not act on
        // is left exactly as the create left it.
        Row {
            case: "one queue's own declaration changed, with the tag set restated",
            stack_name: "tags-unchanged",
            created_with: &[("Owner", "platform")],
            submitted: &[("Owner", "platform")],
            timeout: 45,
            retags: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let body = template(row.stack_name, CREATED_TIMEOUT);
        let mut create = members(&[("StackName", row.stack_name), ("TemplateBody", &body)]);
        create.extend(tag_members(row.created_with));
        cloudformation_ok(&router, case, "CreateStack", &create).await;
        let created = settled_status(&router, case, row.stack_name).await;
        assert_eq!(
            created, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to be created, got {created:?}"
        );
        // What the queues are BEFORE the deployment: the urls they exist under and the instant
        // Amazon SQS created them, neither of which a re-tagging may move.
        let tagged_url = queue_url(&format!("{}-tagged", row.stack_name));
        let other_url = queue_url(&format!("{}-other", row.stack_name));
        let created_at = queue_attribute(&router, case, &tagged_url, "CreatedTimestamp").await;
        assert!(
            created_at.is_some(),
            "case {case:?}: expected Amazon SQS to hold the queue the stack created at \
             {tagged_url}"
        );
        let carried = queue_tags(&router, case, &tagged_url).await;
        let expected = expected_tags(row.created_with, true);
        assert_eq!(
            carried, expected,
            "case {case:?}: a queue a stack creates carries the stack's tags: expected \
             {expected:?}, got {carried:?}"
        );

        // The deployment: the row's tag set, over the row's template.
        let updated_body = template(row.stack_name, row.timeout);
        let mut update = members(&[
            ("StackName", row.stack_name),
            ("TemplateBody", &updated_body),
        ]);
        update.extend(tag_members(row.submitted));
        cloudformation_ok(&router, case, "UpdateStack", &update).await;
        let settled = settled_status(&router, case, row.stack_name).await;
        assert_eq!(
            settled, "UPDATE_COMPLETE",
            "case {case:?}: expected the update to complete, got {settled:?}"
        );

        // The queue whose declaration no row changes: a tag change UPDATES it, and a deployment
        // that leaves the tag set alone does not touch it at all.
        let (status, physical_id) = reported_resource(&router, case, row.stack_name, OTHER).await;
        let expected_status = if row.retags {
            "UPDATE_COMPLETE"
        } else {
            "CREATE_COMPLETE"
        };
        assert_eq!(
            (status.as_str(), physical_id.as_str()),
            (expected_status, other_url.as_str()),
            "case {case:?}: expected {OTHER} at {expected_status} under {other_url}, got \
             {status:?} under {physical_id:?}"
        );
        // ...and the resource no service here tags holds no tags for a tag change to change, so
        // nothing is reported about it whatever the tag set becomes.
        let (handle_status, _) = reported_resource(&router, case, row.stack_name, HANDLE).await;
        assert_eq!(
            handle_status, "CREATE_COMPLETE",
            "case {case:?}: expected {HANDLE} to be left as the create left it, got \
             {handle_status:?}"
        );
        // The first queue is updated either way — by the tag change, or by its own changed
        // declaration — and it is the queue that was there rather than a new one.
        let (tagged_status, tagged_physical) =
            reported_resource(&router, case, row.stack_name, TAGGED).await;
        assert_eq!(
            (tagged_status.as_str(), tagged_physical.as_str()),
            ("UPDATE_COMPLETE", tagged_url.as_str()),
            "case {case:?}: expected {TAGGED} updated in place under {tagged_url}, got \
             {tagged_status:?} under {tagged_physical:?}"
        );
        let after = queue_attribute(&router, case, &tagged_url, "CreatedTimestamp").await;
        assert_eq!(
            after, created_at,
            "case {case:?}: the queue must not be re-created, got {after:?} after {created_at:?}"
        );

        // ...and what Amazon SQS really holds for each queue is the set the stack now states.
        for (logical_id, url, declared) in
            [(TAGGED, &tagged_url, true), (OTHER, &other_url, false)]
        {
            let carried = queue_tags(&router, case, url).await;
            let expected = expected_tags(row.submitted, declared);
            assert_eq!(
                carried, expected,
                "case {case:?}: expected {logical_id} to carry {expected:?}, got {carried:?}"
            );
        }
    }
}

/// The same RULE over a resource the stack ADOPTS rather than creates, which is the flow
/// `cdk import` drives.
///
/// Nothing created such a resource, so the tags CloudFormation gives a resource it manages
/// — the stack's own, and the records naming the stack and the resource's place in it — reach
/// it for the first time as the import brings it under management: that application is the
/// resource's own update, so the adopted resource settles at `UPDATE_COMPLETE` under the
/// identifier it was adopted with, carrying the stack's set, without being re-created — while
/// the stack's own operation stays the import it is.
#[tokio::test]
async fn an_import_tags_the_queue_it_adopted() {
    let router = build_router();
    let case = "a queue adopted by an IMPORT change set";
    let stack = "adopt-then-tag";
    let queue_name = "adopted-outside";
    // The tags the stack carries from the deploy that created it, which the import restates.
    let deployed_with: &[(&str, &str)] = &[("Owner", "the-app")];
    // ...and the set a later deployment states over the resources the stack manages.
    let stated_later: &[(&str, &str)] = &[("Owner", "the-import"), ("Tier", "gold")];

    // The stack as it stands before the import: one resource no service here tags, which is
    // what the import adds the queue to.
    let before = format!(
        r#"{{"Resources":{{"{HANDLE}":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}}}}}}"#
    );
    let mut create = members(&[("StackName", stack), ("TemplateBody", &before)]);
    create.extend(tag_members(deployed_with));
    cloudformation_ok(&router, case, "CreateStack", &create).await;
    let created = settled_status(&router, case, stack).await;
    assert_eq!(
        created, "CREATE_COMPLETE",
        "case {case:?}: expected the stack to be created, got {created:?}"
    );

    // ...and the queue that exists OUTSIDE CloudFormation, with a tag of its own that nothing
    // about the stack states.
    let created_queue = sqs_ok(
        &router,
        case,
        "CreateQueue",
        json!({"QueueName": queue_name, "tags": {"Kept": "from-outside"}}),
    )
    .await;
    let url = created_queue
        .get("QueueUrl")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        url,
        queue_url(queue_name),
        "case {case:?}: expected the queue's own url, got {created_queue}"
    );
    let created_at = queue_attribute(&router, case, &url, "CreatedTimestamp").await;

    // The IMPORT change set adopts it, exactly as `cdk import` does — restating the stack's
    // own tag set, as the tool that deploys the app does.
    let with_the_queue = format!(
        r#"{{"Resources":{{
            "{HANDLE}":{{"Type":"AWS::CloudFormation::WaitConditionHandle"}},
            "{TAGGED}":{{"Type":"AWS::SQS::Queue","DeletionPolicy":"Retain",
                          "Properties":{{"QueueName":"{queue_name}"}}}}}}}}"#
    );
    let mut import = members(&[
        ("StackName", stack),
        ("ChangeSetName", "import"),
        ("ChangeSetType", "IMPORT"),
        ("TemplateBody", &with_the_queue),
        ("ResourcesToImport.member.1.ResourceType", "AWS::SQS::Queue"),
        ("ResourcesToImport.member.1.LogicalResourceId", TAGGED),
        (
            "ResourcesToImport.member.1.ResourceIdentifier.entry.1.key",
            "QueueUrl",
        ),
    ]);
    import.push((
        "ResourcesToImport.member.1.ResourceIdentifier.entry.1.value".to_string(),
        url.clone(),
    ));
    import.extend(tag_members(deployed_with));
    cloudformation_ok(&router, case, "CreateChangeSet", &import).await;
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut plan = String::new();
    while Instant::now() < deadline {
        let body = cloudformation_ok(
            &router,
            case,
            "DescribeChangeSet",
            &members(&[("StackName", stack), ("ChangeSetName", "import")]),
        )
        .await;
        plan = first_element(&body, "Status").unwrap_or_default();
        if plan == "CREATE_COMPLETE" || plan == "FAILED" {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        plan, "CREATE_COMPLETE",
        "case {case:?}: expected the import plan to complete, got {plan:?}"
    );
    cloudformation_ok(
        &router,
        case,
        "ExecuteChangeSet",
        &members(&[("StackName", stack), ("ChangeSetName", "import")]),
    )
    .await;
    let imported = settled_status(&router, case, stack).await;
    assert_eq!(
        imported, "IMPORT_COMPLETE",
        "case {case:?}: expected the executed import to complete as an import, got {imported:?}"
    );
    // The resource it adopted reports the tag application as its own update, under the
    // identifier it was adopted with.
    let (status, physical_id) = reported_resource(&router, case, stack, TAGGED).await;
    assert_eq!(
        (status.as_str(), physical_id.as_str()),
        ("UPDATE_COMPLETE", url.as_str()),
        "case {case:?}: expected the adopted resource at UPDATE_COMPLETE under {url}, got \
         {status:?} under {physical_id:?}"
    );
    // ...and what Amazon SQS holds for it is the stack's set, the tag it carried before the
    // adoption gone: the stack's tags are the tags of the resources it manages.
    let carried = queue_tags(&router, case, &url).await;
    let expected = expected_tags(deployed_with, false);
    assert_eq!(
        carried, expected,
        "case {case:?}: expected the adopted queue to carry {expected:?}, got {carried:?}"
    );

    // The deployment that follows the import states a tag set of its own over the resources
    // the stack manages, the adopted one included.
    let mut update = members(&[("StackName", stack), ("TemplateBody", &with_the_queue)]);
    update.extend(tag_members(stated_later));
    cloudformation_ok(&router, case, "UpdateStack", &update).await;
    let settled = settled_status(&router, case, stack).await;
    assert_eq!(
        settled, "UPDATE_COMPLETE",
        "case {case:?}: expected the deployment after the import to complete, got {settled:?}"
    );
    let (status, physical_id) = reported_resource(&router, case, stack, TAGGED).await;
    assert_eq!(
        (status.as_str(), physical_id.as_str()),
        ("UPDATE_COMPLETE", url.as_str()),
        "case {case:?}: expected the adopted resource updated in place under {url}, got \
         {status:?} under {physical_id:?}"
    );
    // ...and what Amazon SQS holds for the queue is that set: the stack's tags are the tags of
    // the resources it manages.
    let carried = queue_tags(&router, case, &url).await;
    let expected = expected_tags(stated_later, false);
    assert_eq!(
        carried, expected,
        "case {case:?}: expected the adopted queue to carry {expected:?}, got {carried:?}"
    );
    // ...and it is the queue that was adopted rather than a new one: Amazon SQS created it
    // before any of this.
    let after = queue_attribute(&router, case, &url, "CreatedTimestamp").await;
    assert_eq!(
        after, created_at,
        "case {case:?}: the adopted queue must not be re-created, got {after:?} after \
         {created_at:?}"
    );
}
