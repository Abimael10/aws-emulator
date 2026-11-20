// SPDX-License-Identifier: CC0-1.0

//! What CloudFormation does to a resource whose in-place change the service that owns it
//! REFUSED, and how far the stack gets as a result.
//!
//! RULE under test: **CloudFormation puts a resource whose change its owning service refused
//! back by DECLARING the configuration in force before the deployment again in that service
//! — never by restoring its own record of it — so the putting back re-reads everything that
//! configuration points at and FAILS where that service will not take it. Which rollback
//! does it follows from what becomes of the stack: one that rolls back declares it again as
//! part of that rollback, so a refusal there leaves the stack in `UPDATE_ROLLBACK_FAILED`
//! (which takes no further `UpdateStack` and is resumed by `ContinueUpdateRollback`), while
//! one whose caller asked for the failed state to be kept (`DisableRollback`) rests in
//! `UPDATE_FAILED` and puts back the failed resource ALONE, reported on that resource's own
//! `UPDATE_ROLLBACK_IN_PROGRESS`/`UPDATE_ROLLBACK_COMPLETE` events.**
//!
//! Why it matters: a change reaches an owning service as several calls (a function's
//! configuration and its deployment package are two), so a refusal part way through leaves
//! the resource half-changed while the stack's record still states what it HAD. A rollback
//! that trusts that record asks nobody for anything and reports success — telling a client
//! the stack is consistent with a configuration that may no longer be materialisable at all,
//! and never firing the `UPDATE_ROLLBACK_FAILED` branch deployment tooling waits on.
//!
//! Four crates have to agree for the rule to hold (CloudFormation decides and drives the
//! rollback, AWS Lambda applies the calls and refuses the ones it cannot serve, Amazon S3
//! answers for the deployment package, Systems Manager holds the sibling resource whose
//! applied change `DisableRollback` keeps, and IAM answers for the execution role), so the
//! suite lives here rather than in any one crate.
//!
//! The rows differ along what the rule generalises over: WHAT the owning service finds at the
//! previous configuration's code location when the rollback declares it again (the object is
//! gone, it has been replaced by a different package, it is untouched) and WHICH rollback is
//! doing the declaring (the stack's own, or the failed resource's alone under
//! `DisableRollback`). Every expected value is computed here from what the row staged and
//! from the wording the answering services publish — never read back out of the answer being
//! checked.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The CloudFormation awsQuery wire, the request dispatcher and the stack poll this crate
// already writes once.
use crate::cloudformation_completed_stack_output_values::{
    authorization, cloudformation, dispatch, reported_failures, settled_status, Answered, REGION,
};
// ...and the Systems Manager and AWS Lambda wires, written once beside the suite about an
// update whose values that service holds.
use crate::cloudformation_lambda_ssm_sourced_update::{reported_configuration, ssm, trust_policy};
use crate::query_wire::{elements, members_of};

/// The logical ids every template below declares.
const ROLE: &str = "ExecRole";
const FUNCTION: &str = "Fn";
const PARAMETER: &str = "Param";

/// The `StackStatus` values the rule is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_FAILED: &str = "UPDATE_FAILED";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";
const UPDATE_ROLLBACK_FAILED: &str = "UPDATE_ROLLBACK_FAILED";
const UPDATE_ROLLBACK_IN_PROGRESS: &str = "UPDATE_ROLLBACK_IN_PROGRESS";

/// The `ResourceStatus` values it is stated in.
const RESOURCE_UPDATE_FAILED: &str = "UPDATE_FAILED";
const RESOURCE_UPDATE_ROLLBACK_IN_PROGRESS: &str = "UPDATE_ROLLBACK_IN_PROGRESS";
const RESOURCE_UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";
const RESOURCE_UPDATE_ROLLBACK_FAILED: &str = "UPDATE_ROLLBACK_FAILED";

/// The type a stack's OWN events are recorded under, which is what tells them apart from the
/// events about the resources it declares.
const STACK_RESOURCE_TYPE: &str = "AWS::CloudFormation::Stack";

/// The two sentences CloudFormation words on the pair of events that report the rollback of
/// the FAILED RESOURCE ALONE — the rollback a stack that rests in the failed status performs.
///
/// Spelled out here rather than taken from the emulator's own constants, so the suite pins
/// AWS's wording and not the implementation's. Both are fixed text: which resource they are
/// about is the event's own `LogicalResourceId`.
const ROLLING_BACK_SENTENCE: &str = "Rolling back the failed resource only.";
const ROLLBACK_SUCCEEDED_SENTENCE: &str = "Rollback succeeded for the failed resources.";

/// The error code Amazon S3 answers for an object it holds nothing under, which is what the
/// refusal of a package read relays.
const NO_SUCH_KEY: &str = "NoSuchKey";

/// A .zip archive holding no entries: the 22-byte end-of-central-directory record — its
/// signature, then the five counts, the directory's size and offset and a zero comment
/// length, all zero — which is a complete archive a reader opens and finds empty.
const STAGED_PACKAGE: &[u8] = b"PK\x05\x06\
\x00\x00\x00\x00\
\x00\x00\x00\x00\
\x00\x00\x00\x00\
\x00\x00\x00\x00\
\x00\x00";

/// A DIFFERENT complete archive: the same empty directory with a ten-byte archive comment, so
/// it is a readable .zip of its own and is ten bytes longer than [`STAGED_PACKAGE`]. What a
/// row that REPLACES the previous object stages, so the length AWS Lambda reports says
/// whether the rollback fetched the object again or trusted a record.
const REPLACEMENT_PACKAGE: &[u8] = b"PK\x05\x06\
\x00\x00\x00\x00\
\x00\x00\x00\x00\
\x00\x00\x00\x00\
\x00\x00\x00\x00\
\x0a\x00\
0123456789";

/// The key every row deploys the function's first package from, and the key its update points
/// at — one nothing is ever staged under, which is what fails the update.
const STAGED_KEY: &str = "code/v1.zip";
const ABSENT_KEY: &str = "code/does-not-exist.zip";

/// The function's memory size before and after the update that fails: the configuration half
/// of the change, which AWS Lambda takes (it is a call of its own) before the code half it
/// refuses — so this is what says whether the rollback declared the previous configuration
/// again or left the resource half-changed.
const MEMORY_AT_CREATE: u64 = 128;
const MEMORY_IN_THE_UPDATE: u64 = 256;

/// The parameter value before and after that update: the sibling resource whose applied
/// change `DisableRollback` is documented to keep.
const PARAMETER_AT_CREATE: &str = "v1";
const PARAMETER_IN_THE_UPDATE: &str = "v2";

/// The services this rule spans, over ONE engine exactly as `core/router/src/lib.rs` mounts
/// them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: every call carries a credential scope so
    // the router can name the target service, and the signature itself is not under test.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// Call one Amazon S3 operation over its REST surface, path-style — the addressing the
/// emulated service routes on.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &'static [u8]) -> Answered {
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/octet-stream"),
            ("authorization", &authorization("s3")),
        ],
        Bytes::from_static(body),
    )
    .await
}

/// Stage `body` at `bucket`/`key`, asserting Amazon S3 took the write.
async fn stage_object(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    body: &'static [u8],
) {
    let staged = s3(router, "PUT", &format!("/{bucket}/{key}"), body).await;
    assert_eq!(
        staged.status,
        200,
        "case {case:?}: expected Amazon S3 to hold {key:?} in {bucket:?}, got {} with {:?}",
        staged.status,
        staged.text()
    );
}

/// Take `bucket`/`key` away, asserting Amazon S3 took the delete.
async fn delete_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let deleted = s3(router, "DELETE", &format!("/{bucket}/{key}"), b"").await;
    assert!(
        (200..300).contains(&deleted.status),
        "case {case:?}: expected Amazon S3 to delete {key:?} from {bucket:?}, got {} with {:?}",
        deleted.status,
        deleted.text()
    );
}

/// The template every case deploys: the function's execution role, the function whose `Code`
/// names an Amazon S3 location, and a sibling Systems Manager parameter — so a case differs
/// from the one before it only in the three values below.
fn template(bucket: &str, function: &str, parameter: &str, code_key: &str, memory: u64, value: &str) -> String {
    json!({
        "Resources": {
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": { "AssumeRolePolicyDocument": trust_policy() },
            },
            FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "MemorySize": memory,
                    "Role": { "Fn::GetAtt": [ROLE, "Arn"] },
                    "Code": { "S3Bucket": bucket, "S3Key": code_key },
                },
            },
            PARAMETER: {
                "Type": "AWS::SSM::Parameter",
                "Properties": { "Name": parameter, "Type": "String", "Value": value },
            },
        },
    })
    .to_string()
}

/// `CreateStack`/`UpdateStack` for `template`, asserting the submission itself was accepted,
/// and answering the body the call was answered with.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    stack: &str,
    body: &str,
    disable_rollback: bool,
) -> Answered {
    let mut members: Vec<(&str, &str)> = vec![
        ("StackName", stack),
        ("TemplateBody", body),
        ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
    ];
    if disable_rollback {
        members.push(("DisableRollback", "true"));
    }
    let answered = cloudformation(router, action, &members).await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from {action}({stack:?}), got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered
}

/// Every event the stack recorded, OLDEST FIRST, as the three members this rule is about:
/// `(ResourceType, LogicalResourceId, ResourceStatus, ResourceStatusReason)`.
///
/// `DescribeStackEvents` answers most recent first, so the list is reversed into the order the
/// stack went through them — which is the order the rule is stated in.
async fn events(router: &MultiServiceRouter, stack: &str) -> Vec<(String, String, String, String)> {
    let answered = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    let body = answered.text();
    assert_eq!(
        answered.status, 200,
        "expected HTTP 200 from DescribeStackEvents({stack:?}), got {} with {body:?}",
        answered.status
    );
    let first = |member: &str, name: &str| {
        elements(member, name).into_iter().next().unwrap_or_default()
    };
    let mut recorded: Vec<(String, String, String, String)> = members_of(&body)
        .into_iter()
        .map(|member| {
            (
                first(member, "ResourceType"),
                first(member, "LogicalResourceId"),
                first(member, "ResourceStatus"),
                first(member, "ResourceStatusReason"),
            )
        })
        .collect();
    recorded.reverse();
    recorded
}

/// The positions, in that list, of the events about `logical_id` that reached `status`.
fn positions(
    recorded: &[(String, String, String, String)],
    logical_id: &str,
    status: &str,
) -> Vec<usize> {
    recorded
        .iter()
        .enumerate()
        .filter(|(_, (_, named, reported, _))| named == logical_id && reported == status)
        .map(|(at, _)| at)
        .collect()
}

/// Every `(ResourceStatus, ResourceStatusReason)` the stack recorded for `logical_id` whose
/// status names a ROLLBACK, oldest first — the list the per-resource rollback rule is about.
fn rollback_events(
    recorded: &[(String, String, String, String)],
    logical_id: &str,
) -> Vec<(String, String)> {
    recorded
        .iter()
        .filter(|(_, named, status, _)| named == logical_id && status.contains("ROLLBACK"))
        .map(|(_, _, status, reason)| (status.clone(), reason.clone()))
        .collect()
}

/// The reasons of the stack's OWN events that reached `status`.
fn stack_event_reasons(
    recorded: &[(String, String, String, String)],
    status: &str,
) -> Vec<String> {
    recorded
        .iter()
        .filter(|(resource_type, _, reported, _)| {
            resource_type == STACK_RESOURCE_TYPE && reported == status
        })
        .map(|(_, _, _, reason)| reason.clone())
        .collect()
}

/// One number AWS Lambda reports for the function, as its own read answers it.
async fn reported_number(router: &MultiServiceRouter, case: &str, function: &str, member: &str) -> i64 {
    let (status, configuration) = reported_configuration(router, function).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AWS Lambda to report the configuration of {function:?}, got \
         {status} with {configuration}"
    );
    configuration
        .get(member)
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected AWS Lambda to report {member} for {function:?}, got {configuration}")
        })
}

/// The value Systems Manager holds for `name`.
async fn parameter_value(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, answered) = ssm(router, "GetParameter", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Systems Manager to hold {name:?}, got {status} with {answered}"
    );
    answered
        .get("Parameter")
        .and_then(|parameter| parameter.get("Value"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Deploy the stack every case starts from — the function's first package staged, the
/// parameter at its first value — and leave it `CREATE_COMPLETE`.
async fn created_stack(router: &MultiServiceRouter, case: &str, row: &Row) {
    let created = s3(router, "PUT", &format!("/{}", row.bucket), b"").await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected Amazon S3 to create {:?}, got {} with {:?}",
        row.bucket,
        created.status,
        created.text()
    );
    stage_object(router, case, row.bucket, STAGED_KEY, STAGED_PACKAGE).await;
    deploy(
        router,
        case,
        "CreateStack",
        row.stack,
        &template(
            row.bucket,
            row.function,
            row.parameter,
            STAGED_KEY,
            MEMORY_AT_CREATE,
            PARAMETER_AT_CREATE,
        ),
        false,
    )
    .await;
    let (settled, described) = settled_status(router, row.stack).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "case {case:?}: expected {:?} to settle at {CREATE_COMPLETE} before the update, got \
         {settled:?}; the failures it reported: {:?}; last answer {described}",
        row.stack,
        reported_failures(router, row.stack).await
    );
}

/// Submit the update every case makes: the code key pointed at an object nothing is under and
/// the two sibling values changed, which is the update AWS Lambda refuses.
async fn failing_update(router: &MultiServiceRouter, case: &str, row: &Row, disable_rollback: bool) {
    deploy(
        router,
        case,
        "UpdateStack",
        row.stack,
        &template(
            row.bucket,
            row.function,
            row.parameter,
            ABSENT_KEY,
            MEMORY_IN_THE_UPDATE,
            PARAMETER_IN_THE_UPDATE,
        ),
        disable_rollback,
    )
    .await;
}

/// What a row does to the object the PREVIOUS configuration names, before the rollback of the
/// failed update declares that configuration again.
#[derive(Clone, Copy)]
enum PreviousObject {
    /// It is taken away, so the re-declaration has nothing to read.
    Deleted,
    /// It is replaced by a different package, so what the re-declaration reads says whether it
    /// read anything at all.
    Replaced,
    /// It is left exactly as the create deployed it.
    Untouched,
}

/// One row: the names it deploys under, and what it leaves at the previous code location.
struct Row {
    case: &'static str,
    stack: &'static str,
    function: &'static str,
    parameter: &'static str,
    bucket: &'static str,
    previous: PreviousObject,
}

/// Act on the previous object as the row says, and answer the length the function's package
/// then has if the rollback goes through.
async fn stage_previous(router: &MultiServiceRouter, row: &Row) -> usize {
    match row.previous {
        PreviousObject::Deleted => {
            delete_object(router, row.case, row.bucket, STAGED_KEY).await;
            0
        }
        PreviousObject::Replaced => {
            stage_object(router, row.case, row.bucket, STAGED_KEY, REPLACEMENT_PACKAGE).await;
            REPLACEMENT_PACKAGE.len()
        }
        PreviousObject::Untouched => STAGED_PACKAGE.len(),
    }
}

const ROLLED_BACK_ROWS: &[Row] = &[
    Row {
        // The recorded shape: the package the function was deployed from is gone by the time
        // the rollback would put it back.
        case: "the previous package has been deleted",
        stack: "revert-reads-s3-deleted",
        function: "revert-reads-s3-deleted-fn",
        parameter: "/probe/revert-reads-s3/deleted",
        bucket: "revert-reads-s3-deleted-bucket",
        previous: PreviousObject::Deleted,
    },
    Row {
        case: "the previous package has been replaced by a different one",
        stack: "revert-reads-s3-replaced",
        function: "revert-reads-s3-replaced-fn",
        parameter: "/probe/revert-reads-s3/replaced",
        bucket: "revert-reads-s3-replaced-bucket",
        previous: PreviousObject::Replaced,
    },
    Row {
        case: "the previous package is untouched",
        stack: "revert-reads-s3-untouched",
        function: "revert-reads-s3-untouched-fn",
        parameter: "/probe/revert-reads-s3/untouched",
        bucket: "revert-reads-s3-untouched-bucket",
        previous: PreviousObject::Untouched,
    },
];

#[tokio::test]
async fn a_stack_rollback_declares_the_previous_configuration_again_in_the_owning_service() {
    // ONE emulator for the whole table: each row names its own bucket, stack, function and
    // parameter, so the rows cannot reach one another's state.
    let router = emulator();
    for row in ROLLED_BACK_ROWS {
        let case = row.case;
        created_stack(&router, case, row).await;
        let package_after_the_rollback = stage_previous(&router, row).await;
        failing_update(&router, case, row, false).await;

        let (settled, described) = settled_status(&router, row.stack).await;
        let recorded = events(&router, row.stack).await;
        match row.previous {
            // 1. The re-declaration had nothing to read, so the ROLLBACK failed — the stack
            // does not report a rollback nobody could carry out.
            PreviousObject::Deleted => {
                assert_eq!(
                    settled, UPDATE_ROLLBACK_FAILED,
                    "case {case:?}: expected the stack to end at {UPDATE_ROLLBACK_FAILED} for a \
                     previous package the owning service cannot fetch, got {settled:?}; the \
                     failures it reported: {:?}; last answer {described}",
                    reported_failures(&router, row.stack).await
                );
                // The resource failed TWICE: once for the update, once for the rollback that
                // tried to put it back — and the second after the stack's own rollback opened,
                // because it is that rollback's work.
                let failures = positions(&recorded, FUNCTION, RESOURCE_UPDATE_FAILED);
                assert_eq!(
                    failures.len(),
                    2,
                    "case {case:?}: expected the update's failure and the rollback's own, got \
                     {failures:?} in {recorded:?}"
                );
                for (which, reason) in failures
                    .iter()
                    .map(|at| (at, recorded[*at].3.clone()))
                {
                    assert!(
                        reason.contains(NO_SUCH_KEY),
                        "case {case:?}: expected the event at {which} to carry the code Amazon S3 \
                         answers for an object it holds nothing under, got {reason:?}"
                    );
                }
                let opened = positions(&recorded, row.stack, UPDATE_ROLLBACK_IN_PROGRESS);
                assert_eq!(
                    opened.len(),
                    1,
                    "case {case:?}: expected the stack to open its rollback exactly once, got \
                     {opened:?} in {recorded:?}"
                );
                assert!(
                    failures[1] > opened[0],
                    "case {case:?}: expected the rollback's own failure after the stack's \
                     {UPDATE_ROLLBACK_IN_PROGRESS}, got the failures at {failures:?} and that \
                     event at {opened:?} in {recorded:?}"
                );
                // ...and ONE stack-level event reports the failed rollback, naming the resource
                // it could not put back.
                let reasons = stack_event_reasons(&recorded, UPDATE_ROLLBACK_FAILED);
                assert_eq!(
                    reasons.len(),
                    1,
                    "case {case:?}: expected exactly one stack-level {UPDATE_ROLLBACK_FAILED} \
                     event, got {reasons:?} in {recorded:?}"
                );
                assert!(
                    reasons[0].contains(&format!("[{FUNCTION}]")),
                    "case {case:?}: expected that event to name the resource that failed, got \
                     {:?}",
                    reasons[0]
                );
            }
            // 2. The owning service took the previous configuration, so the rollback went
            // through — and what it fetched is what the function now carries.
            PreviousObject::Replaced | PreviousObject::Untouched => {
                assert_eq!(
                    settled, UPDATE_ROLLBACK_COMPLETE,
                    "case {case:?}: expected the stack to roll back for a previous package the \
                     owning service serves, got {settled:?}; the failures it reported: {:?}; \
                     last answer {described}",
                    reported_failures(&router, row.stack).await
                );
                let code_size =
                    reported_number(&router, case, row.function, "CodeSize").await;
                assert_eq!(
                    code_size,
                    package_after_the_rollback as i64,
                    "case {case:?}: expected the function to carry the package the rollback \
                     FETCHED from the previous code location, got {code_size} for the {} bytes \
                     staged there",
                    package_after_the_rollback
                );
                // ...and the configuration half of the change the owning service HAD taken is
                // back where it was, which is what declaring it again is for.
                let memory = reported_number(&router, case, row.function, "MemorySize").await;
                assert_eq!(
                    memory, MEMORY_AT_CREATE as i64,
                    "case {case:?}: expected the rollback to put the configuration the update \
                     had applied back, got MemorySize {memory}"
                );
                // ...and so is the sibling resource the update had changed.
                let held = parameter_value(&router, case, row.parameter).await;
                assert_eq!(
                    held, PARAMETER_AT_CREATE,
                    "case {case:?}: expected the rollback to put the sibling parameter back, got \
                     {held:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_failed_rollback_takes_no_update_and_is_resumed_once_the_package_is_back() {
    let case = "a stack whose rollback could not fetch the previous package";
    let router = emulator();
    let row = Row {
        case,
        stack: "revert-reads-s3-resumed",
        function: "revert-reads-s3-resumed-fn",
        parameter: "/probe/revert-reads-s3/resumed",
        bucket: "revert-reads-s3-resumed-bucket",
        previous: PreviousObject::Deleted,
    };
    created_stack(&router, case, &row).await;
    stage_previous(&router, &row).await;
    failing_update(&router, case, &row, false).await;
    let (settled, described) = settled_status(&router, row.stack).await;
    assert_eq!(
        settled, UPDATE_ROLLBACK_FAILED,
        "case {case:?}: expected the stack to end at {UPDATE_ROLLBACK_FAILED}, got {settled:?}; \
         the failures it reported: {:?}; last answer {described}",
        reported_failures(&router, row.stack).await
    );

    // A stack whose rollback failed takes no further update: another operation has to resolve
    // that rollback first, and the refusal says which state it is in.
    let refused = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", row.stack),
            (
                "TemplateBody",
                &template(
                    row.bucket,
                    row.function,
                    row.parameter,
                    STAGED_KEY,
                    MEMORY_AT_CREATE,
                    PARAMETER_AT_CREATE,
                ),
            ),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    let body = refused.text();
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected the update of a stack in {UPDATE_ROLLBACK_FAILED} to be \
         refused, got {} with {body:?}",
        refused.status
    );
    let stack_id = elements(&described, "StackId")
        .into_iter()
        .next()
        .unwrap_or_default();
    assert!(!stack_id.is_empty(), "case {case:?}: expected the stack's own id in {described}");
    let expected = format!(
        "Stack:{stack_id} is in {UPDATE_ROLLBACK_FAILED} state and can not be updated."
    );
    assert!(
        body.contains(&expected),
        "case {case:?}: expected the refusal to be {expected:?}, got {body:?}"
    );

    // ...and once the previous package is back where the configuration names it, continuing
    // the rollback converges: the resumption asks the owning service for it again.
    stage_object(&router, case, row.bucket, STAGED_KEY, STAGED_PACKAGE).await;
    let continued = cloudformation(
        &router,
        "ContinueUpdateRollback",
        &[("StackName", row.stack)],
    )
    .await;
    assert_eq!(
        continued.status,
        200,
        "case {case:?}: expected HTTP 200 from ContinueUpdateRollback, got {} with {:?}",
        continued.status,
        continued.text()
    );
    let (resumed, described) = settled_status(&router, row.stack).await;
    assert_eq!(
        resumed, UPDATE_ROLLBACK_COMPLETE,
        "case {case:?}: expected the resumed rollback to converge, got {resumed:?}; the failures \
         it reported: {:?}; last answer {described}",
        reported_failures(&router, row.stack).await
    );
    let code_size = reported_number(&router, case, row.function, "CodeSize").await;
    assert_eq!(
        code_size,
        STAGED_PACKAGE.len() as i64,
        "case {case:?}: expected the resumption to have fetched the restored package, got \
         {code_size}"
    );
}

const KEPT_FAILURE_ROWS: &[Row] = &[
    Row {
        // The recorded shape: the previous package is there, so the failed resource alone is
        // put back and the rest of the update stays applied.
        case: "a kept failure whose previous package is there",
        stack: "revert-alone-untouched",
        function: "revert-alone-untouched-fn",
        parameter: "/probe/revert-alone/untouched",
        bucket: "revert-alone-untouched-bucket",
        previous: PreviousObject::Untouched,
    },
    Row {
        case: "a kept failure whose previous package has been deleted",
        stack: "revert-alone-deleted",
        function: "revert-alone-deleted-fn",
        parameter: "/probe/revert-alone/deleted",
        bucket: "revert-alone-deleted-bucket",
        previous: PreviousObject::Deleted,
    },
];

#[tokio::test]
async fn a_kept_failure_rolls_back_the_failed_resource_alone_and_says_so() {
    let router = emulator();
    for row in KEPT_FAILURE_ROWS {
        let case = row.case;
        created_stack(&router, case, row).await;
        stage_previous(&router, row).await;
        failing_update(&router, case, row, true).await;

        // The stack RESTS in the failed status: that is what the caller asked for.
        let (settled, described) = settled_status(&router, row.stack).await;
        assert_eq!(
            settled, UPDATE_FAILED,
            "case {case:?}: expected the stack to rest at {UPDATE_FAILED}, got {settled:?}; the \
             failures it reported: {:?}; last answer {described}",
            reported_failures(&router, row.stack).await
        );
        // ...and the sibling resource the update had already changed keeps the new value,
        // which is the distinction keeping the failed state exists to expose.
        let held = parameter_value(&router, case, row.parameter).await;
        assert_eq!(
            held, PARAMETER_IN_THE_UPDATE,
            "case {case:?}: expected the applied sibling change to be kept, got {held:?}"
        );

        // ...while the FAILED resource alone is put back, reported on its own pair of events
        // with CloudFormation's two sentences.
        let recorded = events(&router, row.stack).await;
        let reported = rollback_events(&recorded, FUNCTION);
        let ended = match row.previous {
            // The owning service took the previous configuration back.
            PreviousObject::Untouched | PreviousObject::Replaced => (
                RESOURCE_UPDATE_ROLLBACK_COMPLETE.to_string(),
                ROLLBACK_SUCCEEDED_SENTENCE.to_string(),
            ),
            // It could not: the putting back re-read the code location, and the object is
            // gone — so the resource reports the failed rollback with that refusal, never a
            // revert nothing carried out.
            PreviousObject::Deleted => (
                RESOURCE_UPDATE_ROLLBACK_FAILED.to_string(),
                String::new(),
            ),
        };
        assert_eq!(
            reported.len(),
            2,
            "case {case:?}: expected the failed resource's own rollback pair, got {reported:?} \
             in {recorded:?}"
        );
        assert_eq!(
            reported[0],
            (
                RESOURCE_UPDATE_ROLLBACK_IN_PROGRESS.to_string(),
                ROLLING_BACK_SENTENCE.to_string()
            ),
            "case {case:?}: expected the opening event to say only the failed resource is being \
             put back, got {:?}",
            reported[0]
        );
        assert_eq!(
            reported[1].0, ended.0,
            "case {case:?}: expected the pair to end at {:?}, got {:?}",
            ended.0, reported[1]
        );
        if ended.1.is_empty() {
            assert!(
                reported[1].1.contains(NO_SUCH_KEY),
                "case {case:?}: expected the failed revert to carry the code Amazon S3 answers \
                 for an object it holds nothing under, got {:?}",
                reported[1].1
            );
        } else {
            assert_eq!(
                reported[1].1, ended.1,
                "case {case:?}: expected the sentence CloudFormation words for a revert that \
                 went through, got {:?}",
                reported[1].1
            );
        }
    }
}
