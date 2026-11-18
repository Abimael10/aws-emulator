// SPDX-License-Identifier: CC0-1.0

//! A log group a CloudFormation stack declares belongs to that STACK: it exists from the
//! moment the stack reports itself created, it is still there after everything else under
//! the same name prefix has been reaped, and it goes away only when the stack does.
//!
//! The rule under test: **a resource a stack declares is created in the service that owns it
//! and held there for the whole of the stack's life — an owning-service read answers for it
//! on every call between the create completing and the stack being deleted, whatever happens
//! to resources of the same type the stack did not declare — and only the stack's own
//! deletion releases it.** For `AWS::Logs::LogGroup` the owning reads and writes are
//! CloudWatch Logs' `DescribeLogGroups`, `CreateLogGroup` and `DeleteLogGroup`, so what a
//! client sees is: `DescribeLogGroups(logGroupNamePrefix=<the declared name>)` answers that
//! one group from `CREATE_COMPLETE` onwards; a round of `DeleteLogGroup` over the groups a
//! client made itself takes those away and leaves the stack's standing; `DeleteStack` is what
//! finally takes it.
//!
//! That is the pair of observations an infrastructure-as-code client makes when it tears down
//! only what it created ad hoc and then expects the deployed estate to be intact — it cannot
//! otherwise tell "released by the stack's deletion" from "never created at all", because
//! both read as an empty prefix. Two crates have to agree for the rule to hold (CloudFormation
//! makes the calls, CloudWatch Logs serves them and answers the reads), so the suite lives
//! here: a crate-local test can only ever see one side of the call.
//!
//! The rows differ along what the rule generalises over: which name form the template states
//! (a path-like name, one carrying every other character the member admits, one whose prefix
//! another group's name extends), whether a retention is declared beside it, how many ad-hoc
//! groups are created and then reaped under the same prefix (none, one, several), and whether
//! the stack declares nothing else or a whole workflow topology beside the group (an execution
//! role, a Lambda function and the state machine that invokes it) — a stack whose other
//! resources are what a deployment is really for must not decide the group's fate either way.
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked.
//!
//! What the neighbouring suites pin and this one does not repeat:
//! `cloudformation_provisions_logs_log_groups` pins the CONFIGURATION a declared group is
//! created with (its retention, class and tags) and that a deployment's changes reach it;
//! `cloudformation_declared_log_group_takes_its_function_logs` pins that a function's events
//! land in the group its stack declared. What only this suite reaches is the OWNERSHIP
//! window: the group's existence across unrelated deletes, and its release on the stack's.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder this test crate writes ONCE (`tests/all/query_wire.rs`):
// "Nothing new should write another one — call this module."
use crate::query_wire::form_encode;
// Waiting for a stack to report a status, and for a deleted one to be gone: the waits this
// crate publishes once, so no two suites can disagree about when a stack operation has
// finished. Both poll the Region this suite signs for.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_provisions_logs_log_groups::wait_for_stack_gone;

/// The Region every request here is signed for — the Region the shared stack waits poll in —
/// and so the Region every resource these stacks declare belongs to.
const REGION: &str = "us-east-1";

/// The `StackStatus` a deployment that created everything it declared reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The capability a template declaring NAMED IAM resources has to be created with: "If you
/// have IAM resources with custom names, you must specify CAPABILITY_NAMED_IAM"
/// (`CreateStack`).
const NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";

/// CloudWatch Logs' `X-Amz-Target` prefix, from its model's service shape
/// (`Logs_20140328`), and the signing name its credential scope carries
/// (`sigv4Name: logs`).
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_SIGNING_NAME: &str = "logs";

/// A retention a declaration may state, from the model's `Days` shape: "Possible values are:
/// 1, 3, 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557,
/// 2922, 3288, and 3653." The rows below use the first of them and one from the middle, and
/// one row states none at all.
const SHORTEST_RETENTION: i64 = 1;
const MIDDLE_RETENTION: i64 = 90;

/// The services these stacks run through, over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them: the calls CloudFormation makes in-process for the resources a template
/// declares are served by the same store this suite reads. Request authentication is off, as
/// in every other suite here; the credential scope is what routes a request.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request and what each handler reads its Region from. The signature is never checked: this
/// router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("cloudformation.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Call one CloudWatch Logs operation over its awsJson1_1 wire.
async fn logs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let host = format!("{LOGS_SIGNING_NAME}.{REGION}.amazonaws.com");
    let http_request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{LOGS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(LOGS_SIGNING_NAME))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Every log group CloudWatch Logs holds under `prefix`, as `DescribeLogGroups` reports them.
async fn described_groups(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<Value> {
    let (status, parsed) = logs(
        router,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": prefix }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeLogGroups({prefix:?}), got {status} with \
         {parsed}"
    );
    parsed
        .get("logGroups")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The NAMES of every log group held under `prefix`, as the set a client compares.
async fn listed_names(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
) -> BTreeSet<String> {
    described_groups(router, case, prefix)
        .await
        .into_iter()
        .filter_map(|group| {
            group
                .get("logGroupName")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// The ONE group held under `name`, which must be exactly that group and nothing else.
async fn the_group(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let groups = described_groups(router, case, name).await;
    let named: Vec<&Value> = groups
        .iter()
        .filter(|group| group.get("logGroupName").and_then(Value::as_str) == Some(name))
        .collect();
    assert_eq!(
        named.len(),
        1,
        "case {case:?}: expected DescribeLogGroups({name:?}) to answer exactly that group, got \
         {groups:?}"
    );
    named[0].clone()
}

/// The set a slice of names reads as.
fn set<I: IntoIterator<Item = String>>(names: I) -> BTreeSet<String> {
    names.into_iter().collect()
}

/// A trust policy admitting `principal` for `sts:AssumeRole`, as a template states one.
fn trust(principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The service principals the stacks' trust policies are written for.
const STATES_PRINCIPAL: &str = "states.amazonaws.com";
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The handler the workflow rows deploy: it raises, which is what the reported deployment's
/// function did. Nothing here invokes it — the rule is about the group's existence, not the
/// workflow's outcome — but the declaration is what a real stack carries.
const FAILING_HANDLER: &str =
    "def lambda_handler(event, context):\n    raise RuntimeError('deliberate failure')\n";

/// The substitution the workflow's definition resolves the deployed function through.
const FUNCTION_PLACEHOLDER: &str = "${FunctionArn}";
const FUNCTION_SUBSTITUTION: &str = "FunctionArn";

/// One row of the table: what a stack declares, and the ad-hoc groups a client then makes and
/// reaps beside it.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The prefix the declared group and every ad-hoc one share — what the client lists.
    prefix: &'static str,
    /// The `LogGroupName` the template declares, which begins with [`Row::prefix`].
    declared: &'static str,
    /// The `RetentionInDays` the declaration states, or `None` for one that states none.
    retention: Option<i64>,
    /// The ad-hoc groups the client creates under the prefix and then reaps.
    ad_hoc: &'static [&'static str],
    /// Whether the stack declares a workflow topology beside the group.
    workflow: bool,
}

/// The template a row deploys: the declared log group, and — for a workflow row — the roles,
/// the Lambda function and the state machine a deployment of a workflow really carries.
fn template(row: &Row) -> String {
    let mut properties = json!({ "LogGroupName": row.declared });
    if let Some(days) = row.retention {
        properties["RetentionInDays"] = json!(days);
    }
    let mut resources = json!({
        "DeclaredGroup": {
            "Type": "AWS::Logs::LogGroup",
            "Properties": properties,
        },
    });
    let mut outputs = json!({
        "GroupName": { "Value": { "Ref": "DeclaredGroup" } },
    });
    if row.workflow {
        let definition = json!({
            "Comment": "one failing task",
            "StartAt": "Work",
            "States": {
                "Work": { "Type": "Task", "Resource": FUNCTION_PLACEHOLDER, "End": true },
            },
        })
        .to_string();
        resources["FunctionRole"] = json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": format!("{}-function", row.stack),
                "AssumeRolePolicyDocument": trust(LAMBDA_PRINCIPAL),
            },
        });
        resources["Function"] = json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": format!("{}-worker", row.stack),
                "Runtime": "python3.12",
                "Handler": "index.lambda_handler",
                "Role": { "Fn::GetAtt": ["FunctionRole", "Arn"] },
                "Code": { "ZipFile": FAILING_HANDLER },
            },
        });
        resources["MachineRole"] = json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": format!("{}-machine", row.stack),
                "AssumeRolePolicyDocument": trust(STATES_PRINCIPAL),
            },
        });
        resources["Machine"] = json!({
            "Type": "AWS::StepFunctions::StateMachine",
            "Properties": {
                "StateMachineName": format!("{}-workflow", row.stack),
                "RoleArn": { "Fn::GetAtt": ["MachineRole", "Arn"] },
                "DefinitionString": definition,
                "DefinitionSubstitutions": {
                    FUNCTION_SUBSTITUTION: { "Fn::GetAtt": ["Function", "Arn"] },
                },
            },
        });
        outputs["MachineArn"] = json!({ "Value": { "Fn::GetAtt": ["Machine", "Arn"] } });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a stack-owned log group",
        "Resources": resources,
        "Outputs": outputs,
    })
    .to_string()
}

/// Deploy `template` as `stack` and answer the `DescribeStacks` body it reached
/// [`CREATE_COMPLETE`] with. The completion is owed from the instant the owning services
/// report their resources ready, which no fixed sleep names: the shared wait looks until they
/// do.
async fn deployed(router: &MultiServiceRouter, case: &str, stack: &str, body: &str) -> String {
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", body),
            ("Capabilities.member.1", NAMED_IAM),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateStack({stack:?}) to be accepted, got {status} with \
         {answered}"
    );
    let (reported, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}; CreateStack answered \
         {answered} and the stack last read as {described}"
    );
    described
}

/// Delete `stack` and wait until its deletion has finished.
async fn destroyed(router: &MultiServiceRouter, case: &str, stack: &str) {
    let (status, answered) = cloudformation(router, "DeleteStack", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack({stack:?}), got {status} with \
         {answered}"
    );
    let (gone, last) = wait_for_stack_gone(router, stack).await;
    assert!(
        gone,
        "case {case:?}: expected the deletion of {stack:?} to finish, it last read as {last}"
    );
}

/// The rows: every form of declaration, against every size of ad-hoc cleanup beside it.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a path-like name with a retention, and three ad-hoc groups reaped beside it",
            stack: "owned-group-path",
            prefix: "/sim2real/scn/path/",
            declared: "/sim2real/scn/path/declared",
            retention: Some(SHORTEST_RETENTION),
            ad_hoc: &[
                "/sim2real/scn/path/ad-hoc-1",
                "/sim2real/scn/path/ad-hoc-2",
                "/sim2real/scn/path/ad-hoc-3",
            ],
            workflow: false,
        },
        Row {
            case: "a name carrying every other character the member admits, one ad-hoc group",
            // `logGroupName` is `@pattern("^[\.\-_/#A-Za-z0-9]+$")`, so the dot, hyphen,
            // underscore and `#` are all admitted beside the slashes and alphanumerics.
            stack: "owned-group-characters",
            prefix: "/sim2real/scn/chars/",
            declared: "/sim2real/scn/chars/declared_group-1.v2#a",
            retention: Some(MIDDLE_RETENTION),
            ad_hoc: &["/sim2real/scn/chars/ad.hoc#1"],
            workflow: false,
        },
        Row {
            case: "a declaration stating no retention, with nothing reaped beside it",
            stack: "owned-group-no-retention",
            prefix: "/sim2real/scn/bare/",
            declared: "/sim2real/scn/bare/declared",
            retention: None,
            ad_hoc: &[],
            workflow: false,
        },
        Row {
            case: "a name another ad-hoc group's name EXTENDS, so the prefix read spans both",
            stack: "owned-group-extended",
            prefix: "/sim2real/scn/extended/declared",
            declared: "/sim2real/scn/extended/declared",
            retention: Some(SHORTEST_RETENTION),
            ad_hoc: &[
                "/sim2real/scn/extended/declared-ad-hoc",
                "/sim2real/scn/extended/declared.ad.hoc",
            ],
            workflow: false,
        },
        Row {
            case: "a whole workflow declared beside the group, two ad-hoc groups reaped",
            stack: "owned-group-workflow",
            prefix: "/sim2real/scn/workflow/",
            declared: "/sim2real/scn/workflow/declared",
            retention: Some(SHORTEST_RETENTION),
            ad_hoc: &[
                "/sim2real/scn/workflow/ad-hoc-1",
                "/sim2real/scn/workflow/ad-hoc-2",
            ],
            workflow: true,
        },
    ]
}

/// The whole ownership window, row by row: the declared group is there the instant the stack
/// completes, survives a round of deletes over the ad-hoc groups beside it, and is released
/// only by the stack's own deletion.
#[tokio::test]
async fn a_stack_declared_log_group_survives_an_ad_hoc_cleanup_and_falls_only_with_the_stack() {
    let router = build_router();

    for row in rows() {
        let case = row.case;
        let declared = row.declared.to_string();

        // Nothing of the prefix exists before the deployment, so every later reading is the
        // stack's own work rather than something left over.
        let before = listed_names(&router, case, row.prefix).await;
        assert!(
            before.is_empty(),
            "case {case:?}: expected nothing under {:?} before the stack, got {before:?}",
            row.prefix
        );

        let described = deployed(&router, case, row.stack, &template(&row)).await;

        // 1. The group exists the moment the stack reports itself created, under the name the
        //    template declared, and the prefix carries nothing else.
        let listed = listed_names(&router, case, row.prefix).await;
        assert_eq!(
            listed,
            set([declared.clone()]),
            "case {case:?}: expected {declared:?} alone under {:?} once the stack completed, \
             got {listed:?} (the stack read as {described})",
            row.prefix
        );
        let group = the_group(&router, case, &declared).await;
        let created_at = group.get("creationTime").and_then(Value::as_i64);
        assert!(
            created_at.is_some(),
            "case {case:?}: expected the declared group to report a creationTime, got {group}"
        );
        let retention = group.get("retentionInDays").and_then(Value::as_i64);
        assert_eq!(
            retention, row.retention,
            "case {case:?}: expected retentionInDays {:?} on the declared group, got \
             {retention:?} in {group}",
            row.retention
        );

        // 2. The client makes its own groups under the same prefix; the read spans all of them.
        for name in row.ad_hoc {
            let (status, answered) = logs(
                &router,
                "CreateLogGroup",
                &json!({ "logGroupName": name }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from CreateLogGroup({name:?}), got {status} \
                 with {answered}"
            );
        }
        let expected_with_ad_hoc = set(std::iter::once(declared.clone())
            .chain(row.ad_hoc.iter().map(|name| (*name).to_string())));
        let listed = listed_names(&router, case, row.prefix).await;
        assert_eq!(
            listed, expected_with_ad_hoc,
            "case {case:?}: expected {expected_with_ad_hoc:?} under {:?} once the ad-hoc groups \
             were made, got {listed:?}",
            row.prefix
        );

        // 3. The cleanup reaps ONLY what the client made ad hoc. The stack's group is
        //    untouched: still listed under the prefix, still the same group, with the
        //    retention its declaration gave it.
        for name in row.ad_hoc {
            let (status, answered) = logs(
                &router,
                "DeleteLogGroup",
                &json!({ "logGroupName": name }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from DeleteLogGroup({name:?}), got {status} \
                 with {answered}"
            );
        }
        let listed = listed_names(&router, case, row.prefix).await;
        assert_eq!(
            listed,
            set([declared.clone()]),
            "case {case:?}: expected the stack-owned group {declared:?} alone to survive the \
             cleanup under {:?} — it is the stack's to release — got {listed:?}",
            row.prefix
        );
        let survivor = the_group(&router, case, &declared).await;
        assert_eq!(
            survivor.get("creationTime").and_then(Value::as_i64),
            created_at,
            "case {case:?}: expected the surviving group to be the one the stack created, got \
             {survivor} where it had been created at {created_at:?}"
        );
        assert_eq!(
            survivor.get("retentionInDays").and_then(Value::as_i64),
            row.retention,
            "case {case:?}: expected the surviving group to keep its declared retention {:?}, \
             got {survivor}",
            row.retention
        );

        // 4. Deleting the stack is what releases it.
        destroyed(&router, case, row.stack).await;
        let listed = listed_names(&router, case, row.prefix).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected nothing under {:?} once the stack was deleted, got \
             {listed:?}",
            row.prefix
        );
    }
}

/// The other half of the same rule: ownership is per STACK. Two stacks declare a group each
/// under one prefix; deleting one releases its own group and leaves the other's standing,
/// until that stack is deleted too.
#[tokio::test]
async fn deleting_one_stack_releases_only_the_group_that_stack_declared() {
    let case = "two stacks declaring a group each under one prefix";
    let router = build_router();
    let prefix = "/sim2real/scn/shared/";

    // (the stack, the group it declares)
    let stacks = [
        ("shared-prefix-first", "/sim2real/scn/shared/first"),
        ("shared-prefix-second", "/sim2real/scn/shared/second"),
    ];

    for (stack, declared) in stacks {
        let row = Row {
            case,
            stack,
            prefix,
            declared,
            retention: Some(MIDDLE_RETENTION),
            ad_hoc: &[],
            workflow: false,
        };
        deployed(&router, case, stack, &template(&row)).await;
    }

    let listed = listed_names(&router, case, prefix).await;
    let both = set(stacks.iter().map(|(_, declared)| (*declared).to_string()));
    assert_eq!(
        listed, both,
        "case {case:?}: expected both declared groups {both:?} under {prefix:?}, got {listed:?}"
    );

    destroyed(&router, case, stacks[0].0).await;
    let listed = listed_names(&router, case, prefix).await;
    assert_eq!(
        listed,
        set([stacks[1].1.to_string()]),
        "case {case:?}: expected only {:?} — the group the other stack owns — to survive the \
         deletion of {:?}, got {listed:?}",
        stacks[1].1,
        stacks[0].0
    );

    destroyed(&router, case, stacks[1].0).await;
    let listed = listed_names(&router, case, prefix).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: expected nothing under {prefix:?} once both stacks were deleted, got \
         {listed:?}"
    );
}
