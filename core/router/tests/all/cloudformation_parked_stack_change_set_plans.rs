// SPDX-License-Identifier: CC0-1.0

//! What an UPDATE change set over a stack PARKED by a failed operation plans, and what
//! executing it does to that stack.
//!
//! Two rules, each held over the class it covers rather than over one recorded call:
//!
//! 1. **A stack whose last operation FAILED and was left standing has not finished applying
//!    the tag set in force to its resources, so a change set over it plans that application
//!    onto every taggable resource it holds** — one `Modify` with `Replacement=False` and
//!    `Scope=["Tags"]` each, even where the submitted revision restates the very set in force.
//!    A stack's tags are the tags of its resources ("CloudFormation also propagates these tags
//!    to resources in the stack"), so the application is outstanding work exactly as the
//!    resource changes that operation could not carry through are; a stack whose last operation
//!    COMPLETED carries the set on every resource it holds, so there a revision restating it
//!    plans nothing.
//! 2. **`PolicyAction` is "the action that will be taken on the physical resource when the
//!    change set is executed", so a change on a logical id the stack holds NO physical resource
//!    for reports none.** The create of such a resource was refused, so nothing was ever made
//!    for the execution to replace, delete, retain or snapshot — the resource is created
//!    instead — while a change that replaces a resource the stack really holds reports the
//!    `ReplaceAnd…` form of that resource's `UpdateReplacePolicy`.
//!
//! Together they are what makes the documented `--no-rollback` recovery path reachable: park on
//! failure, correct the template, redeploy the SAME stack. The third test drives exactly that,
//! through the calls a client makes, and asserts the repair end to end — the stack reaches
//! `UPDATE_COMPLETE` under the id it already had, the resources that were created keep the
//! physical resources they already had, and the one that was refused now exists in the service
//! that OWNS it.
//!
//! Three crates have to agree for any of this to hold — CloudFormation plans and executes the
//! change set, Amazon SQS owns the queue and AWS Systems Manager owns the parameters, and those
//! two are the only sides that can say what really exists — so the suite lives here rather than
//! in any one of them.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers are written ONCE in this crate, and so are the
// Systems Manager and Amazon SQS wire callers.
use crate::cloudformation_lambda_ssm_sourced_update::ssm;
use crate::lambda_sqs_event_source_batching_window::sqs;
use crate::query_wire::{blocks_of, elements, form_encode};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// How long a poll loop waits for a change set's creation or a stack operation to reach a
/// settled state. Generous: what a case asserts is the CONDITION it waits for, never the time
/// it took, so a loaded host cannot fail one.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
/// See [`SETTLE_DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The logical ids every template below declares.
const QUEUE: &str = "Queue";
const KEEP: &str = "Keep";
const BAD: &str = "Bad";

/// The tag every stack here is created with, and the second one a row adds.
const ESTATE_TAG: (&str, &str) = ("estate", "parked-stack");
const SECOND_TAG: (&str, &str) = ("owner", "platform");

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: this router runs with authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000000000000000000000000000000000000000"
    )
}

/// The three services this rule spans, over ONE engine exactly as `core/router/src/lib.rs`
/// mounts them: CloudFormation plans and deploys, Amazon SQS owns the queue, AWS Systems
/// Manager owns the parameters.
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
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        engine,
    ));
    router
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
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
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The name of the parameter the good declaration creates.
fn kept_parameter(stack: &str) -> String {
    format!("/{stack}/keep")
}

/// The name the CORRECTED declaration of the failing resource carries.
fn corrected_parameter(stack: &str) -> String {
    format!("/{stack}/keep-corrected")
}

/// A template declaring a queue, a good parameter and a second parameter whose `Name` property
/// is `bad_name` — the JSON the declaration states for it.
///
/// The second parameter depends on the first, so the deployment reaches it only once the good
/// one is there.
fn template(stack: &str, bad_name: &str) -> String {
    let keep = kept_parameter(stack);
    format!(
        r#"{{"AWSTemplateFormatVersion":"2010-09-09","Resources":{{
"{QUEUE}":{{"Type":"AWS::SQS::Queue","Properties":{{"QueueName":"{stack}-queue"}}}},
"{KEEP}":{{"Type":"AWS::SSM::Parameter","Properties":{{"Name":"{keep}","Type":"String","Value":"keep"}}}},
"{BAD}":{{"Type":"AWS::SSM::Parameter","DependsOn":"{KEEP}","Properties":{{"Name":{bad_name},"Type":"String","Value":"bad"}}}}}}}}"#
    )
}

/// The BROKEN revision: the second parameter asks for the name the good one already carries
/// (`{"Ref": "Keep"}` is that parameter's own name), so Parameter Store refuses its create and
/// the deployment cannot carry it through.
fn broken_template(stack: &str) -> String {
    template(stack, &format!(r#"{{"Ref":"{KEEP}"}}"#))
}

/// The CORRECTED revision: the same declarations, with the second parameter's name no longer
/// colliding.
fn corrected_template(stack: &str) -> String {
    template(stack, &format!(r#""{}""#, corrected_parameter(stack)))
}

/// A third revision, which renames that parameter AGAIN — what a change set over a stack whose
/// create COMPLETED is computed from.
fn renamed_template(stack: &str) -> String {
    template(stack, &format!(r#""/{stack}/keep-renamed""#))
}

/// The `Tags.member.N.Key`/`Value` request members one tag list is sent as.
fn tag_members(tags: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut members = Vec::new();
    for (position, (key, value)) in tags.iter().enumerate() {
        members.push((format!("Tags.member.{}.Key", position + 1), key.to_string()));
        members.push((
            format!("Tags.member.{}.Value", position + 1),
            value.to_string(),
        ));
    }
    members
}

/// One `ResourceChange` as `DescribeChangeSet` reports it.
#[derive(Debug, PartialEq, Eq)]
struct ReportedChange {
    action: String,
    logical_resource_id: String,
    replacement: Option<String>,
    scope: Vec<String>,
    policy_action: Option<String>,
}

/// Every `ResourceChange` an answer carries, ordered by logical id — the order a client that
/// enumerates the plan compares them in.
fn reported_changes(body: &str) -> Vec<ReportedChange> {
    let mut changes: Vec<ReportedChange> = blocks_of(body, "ResourceChange")
        .into_iter()
        .map(|change| ReportedChange {
            action: elements(change, "Action").into_iter().next().unwrap_or_default(),
            logical_resource_id: elements(change, "LogicalResourceId")
                .into_iter()
                .next()
                .unwrap_or_default(),
            replacement: elements(change, "Replacement").into_iter().next(),
            scope: blocks_of(change, "Scope")
                .first()
                .map(|scope| elements(scope, "member"))
                .unwrap_or_default(),
            policy_action: elements(change, "PolicyAction").into_iter().next(),
        })
        .collect();
    changes.sort_by(|left, right| left.logical_resource_id.cmp(&right.logical_resource_id));
    changes
}

/// One change a row expects, spelled as the response carries it.
fn expected(
    logical_resource_id: &str,
    replacement: &str,
    scope: &[&str],
    policy_action: Option<&str>,
) -> ReportedChange {
    ReportedChange {
        action: "Modify".to_string(),
        logical_resource_id: logical_resource_id.to_string(),
        replacement: Some(replacement.to_string()),
        scope: scope.iter().map(|entry| (*entry).to_string()).collect(),
        policy_action: policy_action.map(ToString::to_string),
    }
}

/// Poll `DescribeChangeSet` the way a client's waiter does, and answer the ANSWER the creation
/// settled with.
async fn settled_change_set(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    change_set: &str,
) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = cloudformation(
            router,
            "DescribeChangeSet",
            &[("ChangeSetName", change_set), ("StackName", stack)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeChangeSet must answer 200, got {status} with {body}"
        );
        let reported = elements(&body, "Status").into_iter().next().unwrap_or_default();
        if reported == "CREATE_COMPLETE" || reported == "FAILED" {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the change set {change_set:?} to settle, last saw \
             {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `DescribeStacks` the way a client's waiter does, and answer the ANSWER the stack settled
/// with — the first read whose `StackStatus` is not an operation in flight.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks must answer 200, got {status} with {body}"
        );
        let reported = elements(&body, "StackStatus")
            .into_iter()
            .next()
            .unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {stack:?} to settle, last saw {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create a change set over `stack` and answer the `Id` it was created under, once its creation
/// has settled `CREATE_COMPLETE`.
async fn create_change_set(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    change_set: &str,
    change_set_type: &str,
    template: &str,
    tags: &[(&str, &str)],
) -> String {
    let tags = tag_members(tags);
    let mut members: Vec<(&str, &str)> = vec![
        ("StackName", stack),
        ("ChangeSetName", change_set),
        ("ChangeSetType", change_set_type),
        ("TemplateBody", template),
    ];
    members.extend(
        tags.iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    let (status, body) = cloudformation(router, "CreateChangeSet", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateChangeSet must answer 200, got {status} with {body}"
    );
    let id = elements(&body, "Id").into_iter().next().unwrap_or_default();
    assert!(
        !id.is_empty(),
        "case {case:?}: CreateChangeSet must answer an Id, got {body}"
    );
    let described = settled_change_set(router, case, stack, &id).await;
    let reported = elements(&described, "Status")
        .into_iter()
        .next()
        .unwrap_or_default();
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "case {case:?}: the change set {change_set:?} must complete its creation, got {described}"
    );
    id
}

/// Execute a change set with the failed state asked to be KEPT (`DisableRollback=true`, which
/// is what `cdk deploy --no-rollback` sends), and answer the `StackStatus` the stack settled in.
async fn execute_keeping_the_failed_state(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    change_set: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "ExecuteChangeSet",
        &[
            ("ChangeSetName", change_set),
            ("StackName", stack),
            ("DisableRollback", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ExecuteChangeSet must answer 200, got {status} with {body}"
    );
    let settled = settled_stack(router, case, stack).await;
    elements(&settled, "StackStatus")
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// The parameter Systems Manager holds under `name` as `(Value, Version)`, or `None` where it
/// holds none.
///
/// `None` is only ever the modelled `ParameterNotFound`: any other refusal is this suite's own
/// fault rather than the answer "there is no such parameter", so it fails the case instead of
/// reading as an absence.
async fn held_parameter(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Option<(String, i64)> {
    let (status, parsed) = ssm(router, "GetParameter", &json!({ "Name": name })).await;
    if status == 200 {
        let parameter = parsed.get("Parameter").cloned().unwrap_or(Value::Null);
        let value = parameter
            .get("Value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let version = parameter.get("Version").and_then(Value::as_i64).unwrap_or(0);
        return Some((value, version));
    }
    let refusal = parsed
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        refusal.ends_with("ParameterNotFound"),
        "case {case:?}: GetParameter({name}) must answer the parameter or the modelled \
         ParameterNotFound, got {status} with {parsed}"
    );
    None
}

/// The url Amazon SQS holds the queue `name` under, and the attributes it reports for it — the
/// only side that can say whether the queue exists and whether it is the one that was there
/// before.
async fn held_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Option<(String, BTreeMap<String, String>)> {
    let (status, parsed) = sqs(router, "GetQueueUrl", &json!({ "QueueName": name })).await;
    if status != 200 {
        return None;
    }
    let url = parsed
        .get("QueueUrl")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let (status, parsed) = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["CreatedTimestamp"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes({url}) must answer 200, got {status} with {parsed}"
    );
    let attributes = parsed
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
        .unwrap_or_default();
    Some((url, attributes))
}

/// Deploy `template` as a NEW stack through a `CREATE` change set executed with the failed
/// state asked to be kept, and answer the `StackStatus` it settled in — the shape
/// `cdk deploy --no-rollback` drives.
async fn deploy_a_new_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
    tags: &[(&str, &str)],
) -> String {
    let change_set =
        create_change_set(router, case, stack, "deploy", "CREATE", template, tags).await;
    execute_keeping_the_failed_state(router, case, stack, &change_set).await
}

/// One row of the plan table: the revision a stack is first deployed with, the status that
/// deployment settles the stack in, the revision a second change set submits with the tags it
/// states, and the plan that change set must answer.
struct Row {
    case: &'static str,
    stack: &'static str,
    first: String,
    settled: &'static str,
    second: String,
    first_tags: Vec<(&'static str, &'static str)>,
    second_tags: Vec<(&'static str, &'static str)>,
    expected: Vec<ReportedChange>,
}

/// The class the two rules cover: a stack parked by a failed create whose correcting revision
/// restates the tags in force, one that states them differently, and — the control — a stack
/// whose create COMPLETED, where the same restatement plans nothing about tags and the
/// replacement it does plan acts on a physical resource that is really there.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a stack parked by a failed create, the correcting revision restating its \
                   tags",
            stack: "parked-restated",
            first: broken_template("parked-restated"),
            settled: "CREATE_FAILED",
            second: corrected_template("parked-restated"),
            first_tags: vec![ESTATE_TAG],
            second_tags: vec![ESTATE_TAG],
            expected: vec![
                // The resource that was never made: its name property requires recreation, so
                // the change replaces it — but there is no physical resource to act on.
                expected(BAD, "True", &["Properties"], None),
                expected(KEEP, "False", &["Tags"], None),
                expected(QUEUE, "False", &["Tags"], None),
            ],
        },
        Row {
            case: "a stack parked by a failed create, the correcting revision adding a tag",
            stack: "parked-retagged",
            first: broken_template("parked-retagged"),
            settled: "CREATE_FAILED",
            second: corrected_template("parked-retagged"),
            first_tags: vec![ESTATE_TAG],
            second_tags: vec![ESTATE_TAG, SECOND_TAG],
            expected: vec![
                expected(BAD, "True", &["Properties"], None),
                expected(KEEP, "False", &["Tags"], None),
                expected(QUEUE, "False", &["Tags"], None),
            ],
        },
        Row {
            case: "a stack whose create completed, the next revision restating its tags",
            stack: "completed-restated",
            first: corrected_template("completed-restated"),
            settled: "CREATE_COMPLETE",
            second: renamed_template("completed-restated"),
            first_tags: vec![ESTATE_TAG],
            second_tags: vec![ESTATE_TAG],
            expected: vec![
                // The resource the stack really holds IS replaced, so the plan reports what
                // that replacement does to the physical resource that is there.
                expected(BAD, "True", &["Properties"], Some("ReplaceAndDelete")),
            ],
        },
    ]
}

/// RULES 1 and 2: what an UPDATE change set over each kind of stack plans.
#[tokio::test]
async fn a_change_set_plans_the_outstanding_tag_application_and_acts_only_on_resources_that_exist()
{
    let router = build_router();
    for row in rows() {
        let case = row.case;
        let settled =
            deploy_a_new_stack(&router, case, row.stack, &row.first, &row.first_tags).await;
        assert_eq!(
            settled, row.settled,
            "case {case:?}: expected the first deployment to settle {:?}, got {settled:?}",
            row.settled
        );

        let change_set = create_change_set(
            &router,
            case,
            row.stack,
            "correcting",
            "UPDATE",
            &row.second,
            &row.second_tags,
        )
        .await;
        let described = settled_change_set(&router, case, row.stack, &change_set).await;
        let reported = reported_changes(&described);
        assert_eq!(
            reported, row.expected,
            "case {case:?}: expected the plan {:?}, got {reported:?}",
            row.expected
        );
    }
}

/// RULE 1, the other half: the tag application the plan reports is reported for a stack parked
/// by a failed UPDATE too — the status is what says the application is outstanding, not which
/// operation left it so.
#[tokio::test]
async fn a_stack_parked_by_a_failed_update_is_planned_the_same_tag_application() {
    let router = build_router();
    let case = "a stack parked by a failed update";
    let stack = "parked-by-an-update";

    // A healthy stack first: the queue and the good parameter, with the tag in force.
    let settled = deploy_a_new_stack(
        &router,
        case,
        stack,
        &corrected_template(stack),
        &[ESTATE_TAG],
    )
    .await;
    assert_eq!(
        settled, "CREATE_COMPLETE",
        "case {case:?}: expected the first deployment to complete, got {settled:?}"
    );

    // ...then an update that cannot be carried through, with its failed state kept: the second
    // parameter is pointed at the name the good one already carries.
    let change_set = create_change_set(
        &router,
        case,
        stack,
        "breaking",
        "UPDATE",
        &broken_template(stack),
        &[ESTATE_TAG],
    )
    .await;
    let settled = execute_keeping_the_failed_state(&router, case, stack, &change_set).await;
    assert_eq!(
        settled, "UPDATE_FAILED",
        "case {case:?}: expected the failed update to park the stack, got {settled:?}"
    );

    // The correcting revision restates the very tags in force, and the plan still carries the
    // application the parked operation never finished.
    let change_set = create_change_set(
        &router,
        case,
        stack,
        "correcting",
        "UPDATE",
        &corrected_template(stack),
        &[ESTATE_TAG],
    )
    .await;
    let described = settled_change_set(&router, case, stack, &change_set).await;
    let reported = reported_changes(&described);
    let tagged: Vec<&ReportedChange> = reported
        .iter()
        .filter(|change| change.scope == vec!["Tags".to_string()])
        .collect();
    assert_eq!(
        tagged,
        vec![
            &expected(KEEP, "False", &["Tags"], None),
            &expected(QUEUE, "False", &["Tags"], None),
        ],
        "case {case:?}: expected a tags-only change per taggable resource the stack holds, got \
         {reported:?}"
    );
}

/// The repair the two rules make reachable: the correcting redeploy of a parked stack, executed
/// with its failed state kept, completes on the SAME stack id without re-creating what the
/// failed deployment did create.
#[tokio::test]
async fn the_correcting_redeploy_of_a_parked_stack_completes_on_the_same_stack_id() {
    let router = build_router();
    let case = "the correcting redeploy of a parked stack";
    let stack = "parked-repaired";

    let settled =
        deploy_a_new_stack(&router, case, stack, &broken_template(stack), &[ESTATE_TAG]).await;
    assert_eq!(
        settled, "CREATE_FAILED",
        "case {case:?}: expected the broken deployment to park the stack, got {settled:?}"
    );
    let parked = settled_stack(&router, case, stack).await;
    let stack_id = elements(&parked, "StackId")
        .into_iter()
        .next()
        .unwrap_or_default();
    assert!(
        !stack_id.is_empty(),
        "case {case:?}: the parked stack must report a StackId, got {parked}"
    );
    // What the broken deployment DID make, read from the services that own it.
    let queue_before = held_queue(&router, case, &format!("{stack}-queue")).await;
    assert!(
        queue_before.is_some(),
        "case {case:?}: the queue the broken deployment created must exist in Amazon SQS, got \
         {queue_before:?}"
    );
    let kept_before = held_parameter(&router, case, &kept_parameter(stack)).await;
    assert_eq!(
        kept_before,
        Some(("keep".to_string(), 1)),
        "case {case:?}: the good parameter must be in Parameter Store at its first version, \
         got {kept_before:?}"
    );
    let corrected_before = held_parameter(&router, case, &corrected_parameter(stack)).await;
    assert_eq!(
        corrected_before, None,
        "case {case:?}: the refused parameter must not exist under its corrected name yet, got \
         {corrected_before:?}"
    );

    // ---- The correcting redeploy of the SAME stack, with the failed state kept. ----
    let change_set = create_change_set(
        &router,
        case,
        &stack_id,
        "correcting",
        "UPDATE",
        &corrected_template(stack),
        &[ESTATE_TAG],
    )
    .await;
    let settled = execute_keeping_the_failed_state(&router, case, &stack_id, &change_set).await;
    assert_eq!(
        settled, "UPDATE_COMPLETE",
        "case {case:?}: expected the correcting redeploy to complete the update, got {settled:?}"
    );
    let repaired = settled_stack(&router, case, &stack_id).await;
    let repaired_id = elements(&repaired, "StackId")
        .into_iter()
        .next()
        .unwrap_or_default();
    assert_eq!(
        repaired_id, stack_id,
        "case {case:?}: the repair must stay on the stack id the parked stack had, got \
         {repaired_id:?}"
    );

    // The resources the failed deployment HAD made are the same ones, not re-created...
    let queue_after = held_queue(&router, case, &format!("{stack}-queue")).await;
    assert_eq!(
        queue_after, queue_before,
        "case {case:?}: the queue must be the one that was already there, got {queue_after:?}"
    );
    let kept_after = held_parameter(&router, case, &kept_parameter(stack)).await;
    assert_eq!(
        kept_after,
        Some(("keep".to_string(), 1)),
        "case {case:?}: the good parameter must be untouched, got {kept_after:?}"
    );
    // ...and the one that was refused now exists, under its corrected name.
    let corrected_after = held_parameter(&router, case, &corrected_parameter(stack)).await;
    assert_eq!(
        corrected_after,
        Some(("bad".to_string(), 1)),
        "case {case:?}: the corrected parameter must exist in Parameter Store, got \
         {corrected_after:?}"
    );
}
