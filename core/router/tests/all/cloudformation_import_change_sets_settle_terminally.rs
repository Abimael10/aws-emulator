// SPDX-License-Identifier: CC0-1.0

//! That a CloudFormation resource import ALWAYS ANSWERS: every status a tool waiting on an
//! import polls leaves its non-terminal value, whichever way the import turns out.
//!
//! The rule under test: **a change set whose `ChangeSetType` is `IMPORT` always settles on a
//! terminal `ChangeSetStatus`, and executing one always settles the stack and each resource it
//! holds on a terminal status — an entry of `ResourcesToImport` whose identifier the service
//! that owns the type does not hold settles the creation on `FAILED`, with the reason that
//! owner states for a resource it cannot read, and leaves the stack and its resources exactly
//! as they were, while entries that all resolve settle the creation on `CREATE_COMPLETE` and,
//! once executed, take the stack to `IMPORT_COMPLETE` with every adopted resource reported
//! under the physical id it already carried.**
//!
//! Two crates have to agree for that to hold (CloudFormation plans and executes the import,
//! and Amazon SQS owns the queues and is the only side that can say whether one exists and
//! whether it was left alone), so the suite lives here, over the assembled emulator.
//!
//! Every wait here is a wait for the CONDITION — look again until the status is terminal, or
//! until a deadline far above the transition's own span passes — so the suite fails naming the
//! status nothing settled rather than hanging, which is the failure this rule is about.
//!
//! The rows differ along what the rule generalises over: how many resources one plan adopts
//! (one and two, out of the 0..200 `ResourcesToImport` takes), what the stack already holds
//! (only a wait condition handle, which is the shape with no resource of the imported type at
//! all, and a handle beside a queue the stack created itself), how the stack came into
//! existence (`CreateStack`, and a `CREATE` change set executed onto it), and — on the refusal
//! half — the form of the identifier nothing holds and its POSITION among the entries.
//!
//! The XML readers and the form encoder are this crate's shared ones
//! (`tests/all/query_wire.rs`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{
    blocks_of, element, elements, form_encode, members_of, opt_element, pairs_of,
};

/// The Region every request here is signed for, and so the Region every stack and queue lives
/// in.
const REGION: &str = "us-east-1";

/// The account id every queue url this emulator mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// How long a wait for a clock-owed transition may take before the suite fails rather than
/// hangs: far above the span itself, so neither a loaded host nor a busy test run can decide
/// the outcome. Nothing here asserts how long anything took.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long a wait sleeps between two looks at the resource it is waiting on.
const SETTLE_INTERVAL: Duration = Duration::from_millis(20);

/// The logical id of the wait condition handle every template here declares: a resource of a
/// type NO import names, so every stack below already holds something the import does not act
/// on.
const HANDLE: &str = "Handle";

/// The logical id of the queue a row's stack created ITSELF, where the row carries one.
const OWN: &str = "Own";

/// The logical ids the imports below adopt, in the order a two-entry plan names them.
const ADOPTED: [&str; 2] = ["Queue", "Second"];

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
/// the request and what each handler reads its Region from. The signature is never checked:
/// this router runs with request authentication off.
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

/// `("Member", "value")` as the member list the callers above take.
fn member(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
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
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One SQS call that must be accepted.
async fn sqs_ok(router: &MultiServiceRouter, case: &str, operation: &str, request: Value) -> Value {
    let (status, parsed) = sqs(router, operation, &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// Create one queue OUTSIDE CloudFormation and answer the url it exists under, which is this
/// type's physical resource id ("Ref returns the queue URL") and so the identifier an import
/// adopts it by.
async fn queue_outside_cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    visibility_timeout: &str,
) -> String {
    let created = sqs_ok(
        router,
        case,
        "CreateQueue",
        json!({"QueueName": name, "Attributes": {"VisibilityTimeout": visibility_timeout}}),
    )
    .await;
    let url = created
        .get("QueueUrl")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let expected = format!("https://sqs.{REGION}.amazonaws.com/{ACCOUNT_ID}/{name}");
    assert_eq!(
        url, expected,
        "case {case:?}: expected the queue's own url, got {created}"
    );
    url
}

/// Every attribute Amazon SQS reports for the queue at `url`.
async fn reported_attributes(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
) -> BTreeMap<String, String> {
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
        .map(|attributes| {
            attributes
                .iter()
                .filter_map(|(name, value)| {
                    value.as_str().map(|value| (name.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `status` is one a caller may stop polling on: a status that is neither awaiting an
/// operation (`*_PENDING`) nor reporting one in flight (`*_IN_PROGRESS`).
fn terminal(status: &str) -> bool {
    !status.is_empty() && !status.ends_with("_PENDING") && !status.ends_with("_IN_PROGRESS")
}

/// A `DescribeChangeSet` answer read once the member `settling` names has settled, by LOOKING
/// again until it has or [`SETTLE_DEADLINE`] passes.
///
/// `Status` is the change set's own creation; `ExecutionStatus` is what became of the plan once
/// something executed it. Both are statuses a tool waiting on an import polls, and both have
/// to leave their in-flight values.
async fn settled_change_set_member(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    change_set: &str,
    settling: &str,
) -> String {
    let members = vec![member("StackName", stack), member("ChangeSetName", change_set)];
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = cloudformation_ok(router, case, "DescribeChangeSet", &members).await;
        let status = opt_element(&body, settling).unwrap_or_default();
        if terminal(&status) {
            return body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the {settling} of change set {change_set:?} was still {status:?} \
             after {SETTLE_DEADLINE:?}, so nothing settled it: {body}"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// The `DescribeChangeSet` answer the change set's own CREATION settles on.
async fn settled_change_set(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    change_set: &str,
) -> String {
    settled_change_set_member(router, case, stack, change_set, "Status").await
}

/// The `StackStatus` `DescribeStacks` reports for `stack` right now.
async fn stack_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let body = cloudformation_ok(
        router,
        case,
        "DescribeStacks",
        &[member("StackName", stack)],
    )
    .await;
    element(&body, "StackStatus")
}

/// The terminal status `stack` settles on, waited for by LOOKING again until it is terminal or
/// [`SETTLE_DEADLINE`] passes — never by sleeping for the span a transition takes.
async fn settled_stack_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let status = stack_status(router, case, stack).await;
        if terminal(&status) {
            return status;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {stack} was still {status:?} after {SETTLE_DEADLINE:?}, so nothing \
             settled it"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// `(the terminal ResourceStatus, the PhysicalResourceId)` `DescribeStackResource` reports for
/// one logical id, waited for the same way.
async fn settled_stack_resource(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> (String, String) {
    let members = vec![
        member("StackName", stack),
        member("LogicalResourceId", logical_id),
    ];
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = cloudformation_ok(router, case, "DescribeStackResource", &members).await;
        let status = opt_element(&body, "ResourceStatus").unwrap_or_default();
        if terminal(&status) {
            let physical = opt_element(&body, "PhysicalResourceId").unwrap_or_default();
            return (status, physical);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {logical_id} of {stack} was still {status:?} after \
             {SETTLE_DEADLINE:?}, so nothing settled it: {body}"
        );
        tokio::time::sleep(SETTLE_INTERVAL).await;
    }
}

/// The plan a `DescribeChangeSet` answer carries, as `logical id -> (action, physical id)`,
/// read from the `ResourceChange` block that carries each pair rather than by zipping
/// document-ordered lists: an addition reports no physical id at all.
fn plan_of(xml: &str) -> BTreeMap<String, (String, String)> {
    blocks_of(xml, "ResourceChange")
        .into_iter()
        .filter_map(|change| {
            let logical_id = opt_element(change, "LogicalResourceId")?;
            let action = opt_element(change, "Action")?;
            let physical_id = opt_element(change, "PhysicalResourceId").unwrap_or_default();
            Some((logical_id, (action, physical_id)))
        })
        .collect()
}

/// Every resource the stack manages, as `logical id -> (ResourceStatus, PhysicalResourceId)`.
async fn stack_resources(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, (String, String)> {
    let body = cloudformation_ok(
        router,
        case,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    members_of(&body)
        .into_iter()
        .filter_map(|resource| {
            let logical_id = opt_element(resource, "LogicalResourceId")?;
            let status = opt_element(resource, "ResourceStatus")?;
            let physical_id = opt_element(resource, "PhysicalResourceId").unwrap_or_default();
            Some((logical_id, (status, physical_id)))
        })
        .collect()
}

/// The template body a row deploys: the wait condition handle every stack holds, the queue a
/// row's stack creates ITSELF where it has one, and one declaration per adopted logical id —
/// each carrying the `DeletionPolicy` an imported resource must, and stating no name, so the
/// identifier can only come from the resource that already exists.
fn template_body(own_queue: bool, adopted: &[&str]) -> String {
    let mut resources = Map::new();
    resources.insert(
        HANDLE.to_string(),
        json!({"Type": "AWS::CloudFormation::WaitConditionHandle"}),
    );
    if own_queue {
        resources.insert(
            OWN.to_string(),
            json!({"Type": "AWS::SQS::Queue", "Properties": {}}),
        );
    }
    for logical_id in adopted {
        resources.insert(
            (*logical_id).to_string(),
            json!({
                "Type": "AWS::SQS::Queue",
                "DeletionPolicy": "Retain",
                "Properties": {},
            }),
        );
    }
    json!({"Resources": Value::Object(resources)}).to_string()
}

/// The `ResourcesToImport` members for `entries`, as `(logical id, the identifier the request
/// names it by)` pairs: one entry per adopted resource, in the order the list gives them.
fn resources_to_import(entries: &[(&str, String)]) -> Vec<(String, String)> {
    let mut members = Vec::new();
    for (index, (logical_id, identifier)) in entries.iter().enumerate() {
        let at = index + 1;
        members.push(member(
            &format!("ResourcesToImport.member.{at}.ResourceType"),
            "AWS::SQS::Queue",
        ));
        members.push(member(
            &format!("ResourcesToImport.member.{at}.LogicalResourceId"),
            logical_id,
        ));
        members.push(member(
            &format!("ResourcesToImport.member.{at}.ResourceIdentifier.entry.1.key"),
            "QueueUrl",
        ));
        members.push(member(
            &format!("ResourcesToImport.member.{at}.ResourceIdentifier.entry.1.value"),
            identifier,
        ));
    }
    members
}

/// The not-found reason a resource provider states for an identifier of its type that it
/// cannot read — the `StatusReason` an import that resolves to nothing fails with. The fixed
/// text is the provider's; the variable parts are the type the request named and the
/// identifier it gave.
fn not_found(resource_type: &str, identifier: &str) -> String {
    format!("Resource of type '{resource_type}' with identifier '{identifier}' was not found.")
}

/// One row of the adoption table.
struct AdoptionRow {
    case: &'static str,
    stack_name: &'static str,
    /// Whether the stack is brought into existence by a `CREATE` change set executed onto it,
    /// rather than by `CreateStack`.
    through_a_change_set: bool,
    /// Whether the stack holds a queue it created ITSELF beside the handle.
    own_queue: bool,
    /// The queues created outside CloudFormation that this row adopts, as
    /// `(the queue's name, the VisibilityTimeout it was created with)`.
    queues: &'static [(&'static str, &'static str)],
}

/// RULE: a plan that adopts resources which all resolve settles `CREATE_COMPLETE`, and
/// executing it settles the stack on `IMPORT_COMPLETE` with every adopted resource reported
/// under the identifier it already carried — and the stack-level update a deploying tool
/// follows the import with settles too, leaving the adopted resources exactly as they were.
#[tokio::test]
async fn an_import_that_resolves_settles_the_change_set_the_stack_and_its_resources() {
    let router = build_router();
    let rows: [AdoptionRow; 3] = [
        AdoptionRow {
            case: "a stack holding only a wait condition handle, adopting one queue",
            stack_name: "settles-one",
            through_a_change_set: false,
            own_queue: false,
            queues: &[("settles-one-outside", "45")],
        },
        AdoptionRow {
            case: "a stack that also holds a queue of its own, adopting two queues",
            stack_name: "settles-two",
            through_a_change_set: false,
            own_queue: true,
            queues: &[("settles-two-first", "30"), ("settles-two-second", "60")],
        },
        AdoptionRow {
            case: "a stack a CREATE change set brought into existence, adopting one queue",
            stack_name: "settles-reviewed",
            through_a_change_set: true,
            own_queue: false,
            queues: &[("settles-reviewed-outside", "15")],
        },
    ];

    for row in &rows {
        let case = row.case;
        let before = template_body(row.own_queue, &[]);
        // The stack as it stands before the import, brought into existence the way the row
        // states — and settled, whichever way, before anything is imported into it.
        if row.through_a_change_set {
            cloudformation_ok(
                &router,
                case,
                "CreateChangeSet",
                &[
                    member("StackName", row.stack_name),
                    member("ChangeSetName", "deploy"),
                    member("ChangeSetType", "CREATE"),
                    member("TemplateBody", &before),
                ],
            )
            .await;
            settled_change_set(&router, case, row.stack_name, "deploy").await;
            cloudformation_ok(
                &router,
                case,
                "ExecuteChangeSet",
                &[
                    member("StackName", row.stack_name),
                    member("ChangeSetName", "deploy"),
                ],
            )
            .await;
        } else {
            cloudformation_ok(
                &router,
                case,
                "CreateStack",
                &[
                    member("StackName", row.stack_name),
                    member("TemplateBody", &before),
                ],
            )
            .await;
        }
        let deployed = settled_stack_status(&router, case, row.stack_name).await;
        assert_eq!(
            deployed, "CREATE_COMPLETE",
            "case {case:?}: the stack the import acts on must be deployed, got {deployed:?}"
        );
        let resources_before = stack_resources(&router, case, row.stack_name).await;

        // ...and the queues that exist OUTSIDE CloudFormation, the ones the import adopts.
        let mut entries: Vec<(&str, String)> = Vec::new();
        let mut attributes_before: Vec<BTreeMap<String, String>> = Vec::new();
        for (index, (name, visibility_timeout)) in row.queues.iter().enumerate() {
            let url =
                queue_outside_cloudformation(&router, case, name, visibility_timeout).await;
            attributes_before.push(reported_attributes(&router, case, &url).await);
            entries.push((ADOPTED[index], url));
        }

        // The IMPORT change set, named as `cdk import` names one on a stack it already
        // deployed: the one the deploy left is deleted first, as that tool deletes it.
        let adopted: Vec<&str> = entries.iter().map(|(logical_id, _)| *logical_id).collect();
        let template = template_body(row.own_queue, &adopted);
        if row.through_a_change_set {
            cloudformation_ok(
                &router,
                case,
                "DeleteChangeSet",
                &[
                    member("StackName", row.stack_name),
                    member("ChangeSetName", "deploy"),
                ],
            )
            .await;
        }
        let mut members = vec![
            member("StackName", row.stack_name),
            member("ChangeSetName", "import"),
            member("ChangeSetType", "IMPORT"),
            member("TemplateBody", &template),
        ];
        members.extend(resources_to_import(&entries));
        cloudformation_ok(&router, case, "CreateChangeSet", &members).await;

        // The creation settles, and what it settled on is an executable plan reporting one
        // adoption per entry under the identifier the resource already exists with.
        let settled = settled_change_set(&router, case, row.stack_name, "import").await;
        // One adoption per entry, and nothing on the resources the stack already holds: an
        // import changes none of them.
        let expected_plan: BTreeMap<String, (String, String)> = entries
            .iter()
            .map(|(logical_id, url)| {
                ((*logical_id).to_string(), ("Import".to_string(), url.clone()))
            })
            .collect();
        let plan = plan_of(&settled);
        assert_eq!(
            (
                opt_element(&settled, "Status").unwrap_or_default(),
                opt_element(&settled, "ExecutionStatus").unwrap_or_default(),
                plan.clone(),
            ),
            (
                "CREATE_COMPLETE".to_string(),
                "AVAILABLE".to_string(),
                expected_plan.clone(),
            ),
            "case {case:?}: expected an executable plan reporting {expected_plan:?}, got \
             {plan:?} from {settled}"
        );

        // Executing it settles the stack as an import, and settles every resource it holds.
        cloudformation_ok(
            &router,
            case,
            "ExecuteChangeSet",
            &[
                member("StackName", row.stack_name),
                member("ChangeSetName", "import"),
            ],
        )
        .await;
        let imported = settled_stack_status(&router, case, row.stack_name).await;
        assert_eq!(
            imported, "IMPORT_COMPLETE",
            "case {case:?}: an executed import must complete as an import, got {imported:?}"
        );
        let executed =
            settled_change_set_member(&router, case, row.stack_name, "import", "ExecutionStatus")
                .await;
        assert_eq!(
            opt_element(&executed, "ExecutionStatus").as_deref(),
            Some("EXECUTE_COMPLETE"),
            "case {case:?}: the execution must settle too, got {executed}"
        );
        for (logical_id, url) in &entries {
            let (status, physical) =
                settled_stack_resource(&router, case, row.stack_name, logical_id).await;
            assert_eq!(
                physical, *url,
                "case {case:?}: {logical_id} must be reported under the identifier it was \
                 adopted under, got {physical:?} at {status:?}"
            );
        }
        // The resources the stack already had are still the ones it had: an import adopts,
        // it does not re-deploy.
        let kept = stack_resources(&router, case, row.stack_name).await;
        for (logical_id, (_, physical_before)) in &resources_before {
            let now = kept.get(logical_id).map(|(_, physical)| physical.clone());
            assert_eq!(
                now.as_deref(),
                Some(physical_before.as_str()),
                "case {case:?}: {logical_id} must keep the identity it had, got {now:?} after \
                 {physical_before:?}"
            );
        }

        // ...and the stack-level update a deploying tool follows an import with — `cdk
        // import` applies the stack's tags with one — settles as an update, with every
        // adopted resource left as it was.
        let update = vec![
            member("StackName", row.stack_name),
            member("TemplateBody", &template),
            member("Tags.member.1.Key", "Owner"),
            member("Tags.member.1.Value", "the-import"),
        ];
        cloudformation_ok(&router, case, "UpdateStack", &update).await;
        let updated = settled_stack_status(&router, case, row.stack_name).await;
        assert_eq!(
            updated, "UPDATE_COMPLETE",
            "case {case:?}: the update that follows the import must complete, got {updated:?}"
        );
        for (index, (logical_id, url)) in entries.iter().enumerate() {
            let (status, physical) =
                settled_stack_resource(&router, case, row.stack_name, logical_id).await;
            assert_eq!(
                (physical.as_str(), status.as_str()),
                (url.as_str(), "UPDATE_COMPLETE"),
                "case {case:?}: {logical_id} must settle under its adopted identity, got \
                 {physical:?} at {status:?}"
            );
            let after = reported_attributes(&router, case, url).await;
            let before = &attributes_before[index];
            assert_eq!(
                (
                    after.get("CreatedTimestamp"),
                    after.get("VisibilityTimeout")
                ),
                (
                    before.get("CreatedTimestamp"),
                    before.get("VisibilityTimeout")
                ),
                "case {case:?}: the adopted queue {url} must be neither re-created nor \
                 reconfigured, got {after:?} after {before:?}"
            );
        }
    }
}

/// One row of the refusal table: a plan whose entries name an identifier nothing holds.
struct RefusalRow {
    case: &'static str,
    /// The name this row's change set is created under, so each row's failed plan stays
    /// readable beside the others'.
    change_set: &'static str,
    /// The entries the plan names, as `(logical id, the identifier the request names it by)`.
    entries: Vec<(&'static str, String)>,
    /// The identifier the refusal must name: the FIRST entry, in request order, that resolves
    /// to nothing.
    named: String,
}

/// RULE's converse: an identifier the owning service does not hold is nothing to adopt, so the
/// creation settles on `FAILED` naming the type and that identifier, the failed plan is listed
/// for the stack, and the stack and its resources are left exactly as they were.
#[tokio::test]
async fn an_import_that_resolves_to_nothing_settles_failed_and_leaves_the_stack_alone() {
    let router = build_router();
    let case = "the stack every refused import acts on";
    cloudformation_ok(
        &router,
        case,
        "CreateStack",
        &[
            member("StackName", "refuses"),
            member("TemplateBody", &template_body(true, &[])),
        ],
    )
    .await;
    let deployed = settled_stack_status(&router, case, "refuses").await;
    assert_eq!(
        deployed, "CREATE_COMPLETE",
        "case {case:?}: the stack the refused imports act on must be deployed, got {deployed:?}"
    );
    let resources_before = stack_resources(&router, case, "refuses").await;
    // One queue that really exists, so a plan can name a resolvable entry beside one that
    // resolves to nothing.
    let held = queue_outside_cloudformation(&router, case, "refuses-held", "45").await;
    let absent = format!("https://sqs.{REGION}.amazonaws.com/{ACCOUNT_ID}/never-created");

    let other_account = format!("https://sqs.{REGION}.amazonaws.com/111122223333/elsewhere");
    // The rows differ along the FORM of the identifier nothing holds, how many entries the
    // plan carries, and WHERE among them the unresolvable one sits.
    let rows: Vec<RefusalRow> = vec![
        RefusalRow {
            case: "a queue url of the right form that nothing holds",
            change_set: "absent-url",
            entries: vec![(ADOPTED[0], absent.clone())],
            named: absent.clone(),
        },
        RefusalRow {
            case: "a queue url naming another account's queue",
            change_set: "absent-account",
            entries: vec![(ADOPTED[0], other_account.clone())],
            named: other_account.clone(),
        },
        RefusalRow {
            case: "an identifier that is no queue url at all",
            change_set: "absent-text",
            entries: vec![(ADOPTED[0], "not-a-queue-url".to_string())],
            named: "not-a-queue-url".to_string(),
        },
        RefusalRow {
            case: "two entries, the second of which nothing holds",
            change_set: "absent-second",
            entries: vec![(ADOPTED[0], held.clone()), (ADOPTED[1], absent.clone())],
            named: absent.clone(),
        },
        RefusalRow {
            case: "two entries, the first of which nothing holds",
            change_set: "absent-first",
            entries: vec![(ADOPTED[0], absent.clone()), (ADOPTED[1], held.clone())],
            named: absent.clone(),
        },
    ];

    for row in &rows {
        let RefusalRow {
            case,
            change_set,
            entries,
            named,
        } = row;
        // The template an IMPORT submits declares the resources it adopts and nothing else
        // new, so it is built from the row's own entries.
        let adopted: Vec<&str> = entries.iter().map(|(logical_id, _)| *logical_id).collect();
        let template = template_body(true, &adopted);
        let mut members = vec![
            member("StackName", "refuses"),
            member("ChangeSetName", change_set),
            member("ChangeSetType", "IMPORT"),
            member("TemplateBody", &template),
        ];
        members.extend(resources_to_import(entries));
        cloudformation_ok(&router, case, "CreateChangeSet", &members).await;

        let settled = settled_change_set(&router, case, "refuses", change_set).await;
        let expected = not_found("AWS::SQS::Queue", named);
        assert_eq!(
            (
                opt_element(&settled, "Status").unwrap_or_default(),
                elements(&settled, "StatusReason")
                    .into_iter()
                    .next()
                    .unwrap_or_default(),
            ),
            ("FAILED".to_string(), expected.clone()),
            "case {case:?}: expected a failed plan naming {named:?}, got {settled}"
        );
        // ...and the failed plan is one the stack HOLDS, which is how a deploying tool reads
        // what became of the import it asked for.
        let listed = cloudformation_ok(
            &router,
            case,
            "ListChangeSets",
            &[member("StackName", "refuses")],
        )
        .await;
        let statuses = pairs_of(&listed, "ChangeSetName", "Status");
        assert_eq!(
            statuses.get(*change_set).map(String::as_str),
            Some("FAILED"),
            "case {case:?}: expected the failed change set to be listed, got {statuses:?}"
        );
        // The stack is untouched: the plan was never carried out, and the one queue that does
        // exist was adopted by nothing.
        let after = stack_status(&router, case, "refuses").await;
        assert_eq!(
            after, "CREATE_COMPLETE",
            "case {case:?}: the stack must be left as it was, got {after:?}"
        );
        let resources_after = stack_resources(&router, case, "refuses").await;
        assert_eq!(
            resources_after, resources_before,
            "case {case:?}: the stack's resources must be left as they were, got \
             {resources_after:?} after {resources_before:?}"
        );
    }
}
