// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack publishes for the `AWS::CodePipeline::*` resource family, and
//! what CodePipeline really holds behind it.
//!
//! RULE under test: **a stack that reports a resource of a type as created publishes, for
//! that resource, the identifiers the type's "Return values" section documents — the
//! resource's primary identifier under `Ref` and each documented attribute under
//! `Fn::GetAtt`, every one of them a value the OWNING service answers to — and it reports a
//! resource as created only where that service created it; a create the owning service
//! refuses fails the resource, with that service's own refusal quoted on the resource's
//! event, and rolls the stack back with nothing behind it.**
//!
//! For this family that means: a custom action type's `Ref` is the
//! `Category|Provider|Version` triple every CodePipeline call addresses a registration by
//! (the type declares no name of its own), a pipeline publishes the ARN that service keys it
//! by and the version it allocated, a webhook publishes the trigger URL that service
//! generated — and `ListWebhooks`, `GetPipeline` and `ListActionTypes` each answer for what
//! the stack published.
//!
//! Why it matters: a stack's `Outputs` and its physical resource ids are the handles every
//! client downstream of a deployment uses — a `Ref` passed into another resource's property,
//! an `Export` read by `Fn::ImportValue`, `describe-stacks --query Stacks[0].Outputs` read by
//! a pipeline script. An identifier that names nothing in the owning service, an output with
//! no value at all, or a resource the owning service never created makes the completed stack
//! a false report.
//!
//! Four crates have to agree for the rule to hold (CloudFormation provisions, CodePipeline
//! owns the pipelines, action types and webhooks, IAM answers for the service role and
//! Amazon S3 for the artifact store), so the suite lives here rather than in any one crate.
//!
//! The rows differ along what the rule generalises over: which part of the identifying triple
//! a custom action type declares (its category, its provider and its version), whether the
//! template NAMES the pipeline or leaves the name to CloudFormation, WHAT makes a declared
//! service role one CodePipeline cannot assume (a trust policy admitting another service, one
//! that denies the assume outright, and a role ARN this account's IAM holds nothing under)
//! beside the control that CAN be assumed, and which authentication a webhook declares with
//! how many filter rules.
//!
//! Every expected value is computed here from what the row declares and from what the owning
//! service answers, never read back out of the answer being checked.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`).
use crate::query_wire::{block_of, elements, form_encode, members_of, opt_element, outputs_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// CodePipeline's `X-Amz-Target` prefix, from that service's own model (service shape
/// `CodePipeline_20150709`).
const TARGET_PREFIX: &str = "CodePipeline_20150709";

/// The `ActionOwner` every action type `CreateCustomActionType` registers carries ("used for
/// custom actions only"), which is also the filter `ListActionTypes` reports them under.
const OWNER_CUSTOM: &str = "Custom";

/// The separator CloudFormation joins the parts of a COMPOSITE primary identifier with.
const IDENTITY_SEPARATOR: &str = "|";

/// The `HandlerErrorCode` class a refusal the type's provider has no mapping for is reported
/// under, which is a value of the CloudFormation model's own `HandlerErrorCode` enum.
const GENERAL_SERVICE_EXCEPTION: &str = "GeneralServiceException";

/// The SDK's own name for AWS CodePipeline, as the exception its SDK raises states it (that
/// model's `sdkId`).
const CODEPIPELINE_SERVICE: &str = "CodePipeline";

/// The HTTP status CodePipeline refuses a `CreatePipeline` whose declaration it will not run a
/// pipeline from with: `InvalidStructureException`'s own `httpError`
/// (`smithy_query(query="operation",operation="CreatePipeline")`).
const REFUSED_STRUCTURE_STATUS: u16 = 400;

/// The error code CodePipeline answers for a pipeline it does not hold.
const PIPELINE_NOT_FOUND: &str = "PipelineNotFoundException";

/// How long a wait for a stack to reach a terminal status is given.
///
/// Generous on purpose: what is waited FOR is the deployment settling, which is on the
/// stack's own clock, and every wait below ends as soon as the condition holds rather than
/// after a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly as
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
    router.register_service(
        service_com_amazonaws_codepipeline_codepipeline_20150709::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one awsQuery operation of `service` at `version`.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// Call one IAM operation over its awsQuery wire.
async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "iam", "2010-05-08", action, members).await
}

/// Call one CodePipeline operation over the awsJson1_1 wire its model declares.
async fn codepipeline(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("codepipeline.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("codepipeline"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Deploy `template` as `stack`, asserting the submission itself was accepted.
async fn create_stack(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
    capability: &str,
) {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", capability),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
}

/// Poll `stack` until its status is no longer an `*_IN_PROGRESS` one, and answer the status it
/// settled in together with the `DescribeStacks` answer that reported it.
///
/// The wait is on that CONDITION — the deployment reaching a terminal status — rather than on
/// a fixed span, so it ends as soon as the stack has settled.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reached = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") {
            return (reached, body);
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {stack} was still {reached:?} after {SETTLE_DEADLINE:?}, so nothing \
             settled it: {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The physical resource id the stack reports for one logical id.
async fn physical_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "DescribeStackResources",
        &[("StackName", stack), ("LogicalResourceId", logical_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {body}"
    );
    elements(&body, "PhysicalResourceId")
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a physical id for {logical_id}, got {body}")
        })
}

/// Every output of a settled stack, as the key/value map a client reads it as — asserting
/// first that EVERY output the stack reports carries a value.
///
/// A completed stack's `Outputs` is what a client reads a deployment's published identifiers
/// from, and `Output` is `{OutputKey, OutputValue, …}`: an entry with a key and no value is
/// one a client cannot use at all (`o["OutputValue"]` raises), so the two readings are
/// compared here rather than letting the value-less entry be silently dropped.
fn resolved_outputs(
    case: &str,
    described: &str,
) -> std::collections::BTreeMap<String, String> {
    let block = block_of(described, "Outputs").unwrap_or_else(|| {
        panic!("case {case:?}: expected the stack to report its Outputs, got {described}")
    });
    let declared: BTreeSet<String> = elements(block, "OutputKey").into_iter().collect();
    let resolved = outputs_of(block);
    let valued: BTreeSet<String> = resolved.keys().cloned().collect();
    assert_eq!(
        valued, declared,
        "case {case:?}: every output of a completed stack carries a resolved OutputValue; \
         these reported a key and no value: {:?} in {described}",
        declared.difference(&valued).collect::<Vec<&String>>()
    );
    resolved
}

/// One output's value, or a panic naming what was reported.
fn output(
    case: &str,
    outputs: &std::collections::BTreeMap<String, String>,
    key: &str,
) -> String {
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the output {key}, got {outputs:?}"))
}

/// The `ResourceStatusReason` of the event `logical_id` reached `reported` in, looked for
/// again until the stack's own clock has recorded it or [`SETTLE_DEADLINE`] passes.
///
/// Read from the `<member>` that carries the logical id, the status and the reason together,
/// so a reason is never read off another resource's — or another transition's — event.
async fn failure_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    reported: &str,
) -> String {
    let giving_up_at = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body \
             {body}"
        );
        let found = members_of(&body).into_iter().find_map(|member| {
            let named = elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .is_some_and(|named| named == logical_id);
            let failed = elements(member, "ResourceStatus")
                .into_iter()
                .next()
                .is_some_and(|status| status == reported);
            (named && failed)
                .then(|| elements(member, "ResourceStatusReason").into_iter().next())
                .flatten()
                .filter(|reason| !reason.is_empty())
        });
        if let Some(reason) = found {
            return reason;
        }
        assert!(
            tokio::time::Instant::now() < giving_up_at,
            "case {case:?}: {logical_id} reported no {reported} reason within \
             {SETTLE_DEADLINE:?}: {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The provider exception a `ResourceStatusReason` quotes, and the `HandlerErrorCode` class it
/// reports beside it.
fn quoted_and_class(case: &str, reason: &str) -> (String, String) {
    const OPENS: &str = "Resource handler returned message: \"";
    const TOKEN: &str = "\" (RequestToken: ";
    const CLASS: &str = "HandlerErrorCode: ";
    let rest = reason.strip_prefix(OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open with {OPENS:?}, got {reason:?}")
    });
    let (quoted, tail) = rest.split_once(TOKEN).unwrap_or_else(|| {
        panic!("case {case:?}: expected a quoted exception and a RequestToken, got {reason:?}")
    });
    let class = tail
        .split_once(CLASS)
        .and_then(|(_, class)| class.strip_suffix(')'))
        .unwrap_or_else(|| panic!("case {case:?}: expected a HandlerErrorCode, got {reason:?}"));
    (quoted.to_string(), class.to_string())
}

/// Hold one quoted exception to the shape a CodePipeline SDK raises — the service's own
/// sentence, then the diagnostic of the refused call (the SDK's name for the service, the
/// status it answered, the id of the call and the attempts the SDK made at it) — and answer
/// the sentence.
fn asserted_sentence(case: &str, quoted: &str, status: u16) -> String {
    const DIAGNOSTIC: &str = " (Service: ";
    const ATTEMPTS: &str = ") (SDK Attempt Count: ";
    let (sentence, diagnostic) = quoted.rsplit_once(DIAGNOSTIC).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK diagnostic inside the quoted exception, after the \
             service's own sentence, got {quoted:?}"
        )
    });
    let (envelope, attempts) = diagnostic.split_once(ATTEMPTS).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the attempt count this service's SDK words after the \
             envelope, got {quoted:?}"
        )
    });
    let fields: Vec<&str> = envelope.split(", ").collect();
    assert_eq!(
        fields.len(),
        3,
        "case {case:?}: expected the service, the status and the call's id and nothing else, got \
         {envelope:?}"
    );
    assert_eq!(
        fields[0], CODEPIPELINE_SERVICE,
        "case {case:?}: expected the SDK's own name for the service, got {:?}",
        fields[0]
    );
    assert_eq!(
        fields[1],
        format!("Status Code: {status}"),
        "case {case:?}: expected the status the owning service answered, got {:?}",
        fields[1]
    );
    assert!(
        fields[2].starts_with("Request ID: ") && fields[2].len() > "Request ID: ".len(),
        "case {case:?}: expected the id of the call, got {:?}",
        fields[2]
    );
    let counted = attempts
        .strip_suffix(')')
        .and_then(|counted| counted.parse::<u32>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a whole attempt count, got {attempts:?}")
        });
    assert!(
        counted >= 1,
        "case {case:?}: expected at least the one attempt the SDK was asked for, got {counted}"
    );
    sentence.to_string()
}

/// A trust policy with ONE statement, as the three parts a row varies.
fn trust(effect: &str, principal: &str, action: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "Service": principal },
            "Action": action
        }]
    })
}

/// The trust policy a pipeline's service role has to carry, taken from the owning service's
/// own principal rather than restated here.
fn assumable_by_codepipeline() -> Value {
    trust(
        "Allow",
        service_com_amazonaws_codepipeline_codepipeline_20150709::state::SERVICE_PRINCIPAL,
        "sts:AssumeRole",
    )
}

/// Create one IAM role carrying `document` as its trust policy, and answer the ARN IAM minted.
async fn create_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    document: &Value,
) -> String {
    let (status, body) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &document.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateRole, got {status} with body {body}"
    );
    elements(&body, "Arn")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the role's ARN, got {body}"))
}

/// The two stages a pipeline declaration carries: an Amazon S3 source action producing one
/// artifact, and a deploy action consuming it — the smallest structure CodePipeline accepts
/// (at least two stages, at least one action outside the `Source` category).
fn stages(bucket: Value) -> Value {
    json!([
        {
            "Name": "Source",
            "Actions": [{
                "Name": "Fetch",
                "ActionTypeId": {
                    "Category": "Source", "Owner": "AWS", "Provider": "S3", "Version": "1"
                },
                "Configuration": {
                    "S3Bucket": bucket,
                    "S3ObjectKey": "source.zip",
                    "PollForSourceChanges": "false"
                },
                "OutputArtifacts": [{ "Name": "SourceOut" }],
                "RunOrder": 1
            }]
        },
        {
            "Name": "Deploy",
            "Actions": [{
                "Name": "Ship",
                "ActionTypeId": {
                    "Category": "Deploy", "Owner": "AWS", "Provider": "S3", "Version": "1"
                },
                "Configuration": {
                    "BucketName": bucket,
                    "ObjectKey": "shipped.zip",
                    "Extract": "false"
                },
                "InputArtifacts": [{ "Name": "SourceOut" }],
                "RunOrder": 1
            }]
        }
    ])
}

// ---------------------------------------------------------------------------
// AWS::CodePipeline::CustomActionType: the composite primary identifier
// ---------------------------------------------------------------------------

/// A custom action type's `Ref` is the `Category|Provider|Version` triple that identifies the
/// registration — the three values every CodePipeline call addresses one by, and the three
/// `ListActionTypes` keys the account's custom action types by — and the stack reports that
/// same triple as the resource's physical id.
///
/// The rows differ along every part of the triple the rule generalises over: the category, the
/// provider and the version a declaration states.
#[tokio::test]
async fn a_custom_action_types_ref_is_the_triple_that_identifies_the_registration() {
    // (case, Category, Provider, Version)
    let rows: [(&str, &str, &str, &str); 3] = [
        ("a Build category at version 1", "Build", "cpfam-build", "1"),
        ("a Test category at version 2", "Test", "cpfam-test", "2"),
        ("a Deploy category at version 11", "Deploy", "cpfam-ship", "11"),
    ];
    let router = build_router();

    for (case, category, provider, version) in rows {
        let stack = format!("cpfam-cat-{provider}");
        let template = json!({
            "Resources": {
                "ActionType": {
                    "Type": "AWS::CodePipeline::CustomActionType",
                    "Properties": {
                        "Category": category,
                        "Provider": provider,
                        "Version": version,
                        "InputArtifactDetails": { "MinimumCount": 1, "MaximumCount": 1 },
                        "OutputArtifactDetails": { "MinimumCount": 0, "MaximumCount": 1 }
                    }
                }
            },
            "Outputs": {
                "ActionTypeRef": { "Value": { "Ref": "ActionType" } }
            }
        })
        .to_string();

        create_stack(&router, case, &stack, &template, "CAPABILITY_IAM").await;
        let (status, described) = settled(&router, case, &stack).await;
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: expected the registration to be created, got {status:?} with \
             {described}"
        );

        // The identifier the type documents: the three declared values, joined.
        let identity = [category, provider, version].join(IDENTITY_SEPARATOR);
        let outputs = resolved_outputs(case, &described);
        let referenced = output(case, &outputs, "ActionTypeRef");
        assert_eq!(
            referenced, identity,
            "case {case:?}: expected Ref to resolve to the composite primary identifier, got \
             {referenced:?}"
        );
        let reported = physical_id(&router, case, &stack, "ActionType").await;
        assert_eq!(
            reported, identity,
            "case {case:?}: expected the physical resource id to be that same identifier, got \
             {reported:?}"
        );

        // And the registration CodePipeline holds is keyed by exactly that triple, so the
        // identifier the stack published names the action type the catalog reports.
        let (code, listed) = codepipeline(
            &router,
            "ListActionTypes",
            &json!({ "actionOwnerFilter": OWNER_CUSTOM }),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected the catalog to answer, got {code} with {listed}"
        );
        let held: Vec<String> = listed["actionTypes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|action_type| {
                let id = &action_type["id"];
                Some(
                    [
                        id["category"].as_str()?,
                        id["provider"].as_str()?,
                        id["version"].as_str()?,
                    ]
                    .join(IDENTITY_SEPARATOR),
                )
            })
            .collect();
        assert!(
            held.contains(&identity),
            "case {case:?}: expected the catalog to hold the action type the stack published \
             ({identity:?}), got {held:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// AWS::CodePipeline::Pipeline: the published ARN and version
// ---------------------------------------------------------------------------

/// The template the pipeline rows deploy: an artifact bucket, a service role and a pipeline
/// wired to both, with every value the type publishes declared as an output.
fn pipeline_template(pipeline_name: Option<&str>) -> String {
    let mut pipeline = json!({
        "RoleArn": { "Fn::GetAtt": ["PipelineRole", "Arn"] },
        "ArtifactStore": { "Type": "S3", "Location": { "Ref": "Store" } },
        "Stages": stages(json!({ "Ref": "Store" }))
    });
    if let Some(name) = pipeline_name {
        pipeline["Name"] = json!(name);
    }
    json!({
        "Resources": {
            "Store": {
                "Type": "AWS::S3::Bucket",
                "Properties": { "VersioningConfiguration": { "Status": "Enabled" } }
            },
            "PipelineRole": {
                "Type": "AWS::IAM::Role",
                "Properties": { "AssumeRolePolicyDocument": assumable_by_codepipeline() }
            },
            "Pipeline": {
                "Type": "AWS::CodePipeline::Pipeline",
                "Properties": pipeline
            }
        },
        "Outputs": {
            "PipelineRef": { "Value": { "Ref": "Pipeline" } },
            "PipelineArn": { "Value": { "Fn::GetAtt": ["Pipeline", "Arn"] } },
            "PipelineVersion": { "Value": { "Fn::GetAtt": ["Pipeline", "Version"] } },
            "RoleArn": { "Value": { "Fn::GetAtt": ["PipelineRole", "Arn"] } }
        }
    })
    .to_string()
}

/// A pipeline stack's outputs each carry a resolved value, and each is a handle the owning
/// service answers to: `Ref` is the pipeline name `GetPipeline` takes, `Arn` is the ARN
/// CodePipeline keys the pipeline by, `Version` is the version that service allocated, and the
/// role's `Arn` is the one IAM minted.
///
/// The rows differ along whether the template NAMES the pipeline or leaves the name to
/// CloudFormation.
#[tokio::test]
async fn a_pipeline_stack_publishes_handles_codepipeline_answers_to() {
    // (case, the name the template declares for the pipeline)
    let rows: [(&str, Option<&str>); 2] = [
        ("a pipeline the template names", Some("cpfam-named-pipeline")),
        ("a pipeline CloudFormation names", None),
    ];
    let router = build_router();

    for (case, declared) in rows {
        let stack = match declared {
            Some(_) => "cpfam-pipeline-named",
            None => "cpfam-pipeline-generated",
        };
        let template = pipeline_template(declared);
        create_stack(&router, case, stack, &template, "CAPABILITY_IAM").await;
        let (status, described) = settled(&router, case, stack).await;
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: expected the pipeline to be created, got {status:?} with {described}"
        );

        let outputs = resolved_outputs(case, &described);
        let reported = physical_id(&router, case, stack, "Pipeline").await;
        let published = output(case, &outputs, "PipelineRef");
        assert_eq!(
            published, reported,
            "case {case:?}: expected Ref to resolve to the pipeline's own name, got \
             {published:?}"
        );
        if let Some(name) = declared {
            assert_eq!(
                published, name,
                "case {case:?}: a pipeline the template names keeps that name, got {published:?}"
            );
        }

        // What CodePipeline itself holds for that name: the ARN it keys the pipeline by and
        // the version it allocated, which is what the stack's outputs have to be.
        let (code, held) = codepipeline(&router, "GetPipeline", &json!({ "name": published })).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected GetPipeline({published:?}) to answer, got {code} with {held}"
        );
        let arn = held["metadata"]["pipelineArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !arn.is_empty(),
            "case {case:?}: expected CodePipeline to report the pipeline's ARN, got {held}"
        );
        assert_eq!(
            output(case, &outputs, "PipelineArn"),
            arn,
            "case {case:?}: expected the published Arn to be the one CodePipeline keys the \
             pipeline by, got {outputs:?}"
        );
        let version = held["pipeline"]["version"].to_string();
        assert_eq!(
            output(case, &outputs, "PipelineVersion"),
            version,
            "case {case:?}: expected the published Version to be the one CodePipeline \
             allocated, got {outputs:?}"
        );

        // And the role the pipeline runs as is the one IAM holds under the published ARN.
        let role = physical_id(&router, case, stack, "PipelineRole").await;
        let (code, described_role) = iam(&router, "GetRole", &[("RoleName", role.as_str())]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected GetRole({role:?}) to answer, got {code} with \
             {described_role}"
        );
        let role_arn = elements(&described_role, "Arn")
            .into_iter()
            .next()
            .unwrap_or_default();
        assert_eq!(
            output(case, &outputs, "RoleArn"),
            role_arn,
            "case {case:?}: expected the published RoleArn to be the one IAM minted, got \
             {outputs:?}"
        );
        assert_eq!(
            held["pipeline"]["roleArn"].as_str(),
            Some(role_arn.as_str()),
            "case {case:?}: expected the pipeline to run as the stack's role, got {held}"
        );
    }
}

// ---------------------------------------------------------------------------
// AWS::CodePipeline::Pipeline: a service role CodePipeline cannot assume
// ---------------------------------------------------------------------------

/// A pipeline whose declared service role CodePipeline cannot assume is a resource that
/// service refuses, so the stack rolls back with nothing behind it and the resource's event
/// quotes that refusal — while a role it CAN assume reaches `CREATE_COMPLETE`.
///
/// The rows differ along WHAT makes the role one that service cannot assume: a trust policy
/// admitting another service, one that denies the assume outright, and a role ARN this
/// account's IAM holds nothing under — beside the control that admits CodePipeline.
#[tokio::test]
async fn a_pipeline_role_codepipeline_cannot_assume_rolls_the_stack_back() {
    /// One row: what the role's trust policy says, and whether the deployment completes.
    struct Row {
        case: &'static str,
        stack: &'static str,
        pipeline: &'static str,
        role: &'static str,
        /// `None` for a row whose role is never created in IAM at all.
        trust: Option<fn() -> Value>,
        completes: bool,
    }
    let rows: &[Row] = &[
        Row {
            case: "a trust policy admitting another service",
            stack: "cpfam-role-other-service",
            pipeline: "cpfam-role-other-service",
            role: "cpfam-lambda-trusting-role",
            trust: Some(|| trust("Allow", "lambda.amazonaws.com", "sts:AssumeRole")),
            completes: false,
        },
        Row {
            case: "a trust policy denying CodePipeline the assume",
            stack: "cpfam-role-denied",
            pipeline: "cpfam-role-denied",
            role: "cpfam-denying-role",
            trust: Some(|| {
                trust(
                    "Deny",
                    service_com_amazonaws_codepipeline_codepipeline_20150709::state::SERVICE_PRINCIPAL,
                    "sts:AssumeRole",
                )
            }),
            completes: false,
        },
        Row {
            case: "a role ARN this account's IAM holds nothing under",
            stack: "cpfam-role-absent",
            pipeline: "cpfam-role-absent",
            role: "cpfam-no-such-role",
            trust: None,
            completes: false,
        },
        Row {
            case: "the control: a trust policy admitting CodePipeline",
            stack: "cpfam-role-admitted",
            pipeline: "cpfam-role-admitted",
            role: "cpfam-admitting-role",
            trust: Some(assumable_by_codepipeline),
            completes: true,
        },
    ];
    let router = build_router();

    for row in rows {
        let case = row.case;
        let role_arn = match row.trust {
            Some(document) => create_role(&router, case, row.role, &document()).await,
            None => format!("arn:aws:iam::{ACCOUNT}:role/{}", row.role),
        };
        let template = json!({
            "Resources": {
                "Store": { "Type": "AWS::S3::Bucket", "Properties": {} },
                "Pipeline": {
                    "Type": "AWS::CodePipeline::Pipeline",
                    "Properties": {
                        "Name": row.pipeline,
                        "RoleArn": &role_arn,
                        "ArtifactStore": { "Type": "S3", "Location": { "Ref": "Store" } },
                        "Stages": stages(json!({ "Ref": "Store" }))
                    }
                }
            }
        })
        .to_string();

        create_stack(&router, case, row.stack, &template, "CAPABILITY_IAM").await;
        let (status, described) = settled(&router, case, row.stack).await;
        if row.completes {
            assert_eq!(
                status, "CREATE_COMPLETE",
                "case {case:?}: expected a role CodePipeline can assume to deploy, got \
                 {status:?} with {described}"
            );
            let (code, held) =
                codepipeline(&router, "GetPipeline", &json!({ "name": row.pipeline })).await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected the pipeline to exist, got {code} with {held}"
            );
            continue;
        }

        assert_eq!(
            status, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the refused pipeline to roll the stack back, got \
             {status:?} with {described}"
        );

        // The resource's own event quotes CodePipeline's refusal as that service's SDK
        // raises it, with the class a refusal the provider has no mapping for falls in.
        let reason = failure_reason(&router, case, row.stack, "Pipeline", "CREATE_FAILED").await;
        let (quoted, class) = quoted_and_class(case, &reason);
        let sentence = asserted_sentence(case, &quoted, REFUSED_STRUCTURE_STATUS);
        assert_eq!(
            sentence,
            service_com_amazonaws_codepipeline_codepipeline_20150709::state::
                role_not_assumable_message(&role_arn),
            "case {case:?}: expected the sentence CodePipeline words for a role it cannot \
             assume, naming the declared role, got {sentence:?} in {reason:?}"
        );
        assert_eq!(
            class, GENERAL_SERVICE_EXCEPTION,
            "case {case:?}: expected the class this provider reports such a refusal under, got \
             {class:?} in {reason:?}"
        );

        // Nothing was left behind: CodePipeline holds no pipeline of the declared name.
        let (code, answered) =
            codepipeline(&router, "GetPipeline", &json!({ "name": row.pipeline })).await;
        assert_eq!(
            code, REFUSED_STRUCTURE_STATUS,
            "case {case:?}: expected GetPipeline on the declared name to be refused, got {code} \
             with {answered}"
        );
        let reported_code = answered["__type"].as_str().unwrap_or_default();
        assert!(
            reported_code.ends_with(PIPELINE_NOT_FOUND),
            "case {case:?}: expected {PIPELINE_NOT_FOUND}, got {answered}"
        );
        assert_eq!(
            answered["message"].as_str(),
            Some(
                service_com_amazonaws_codepipeline_codepipeline_20150709::state::
                    pipeline_not_found_message(
                        service_com_amazonaws_codepipeline_codepipeline_20150709::state::
                            PipelineNotFoundWording::Definition,
                        row.pipeline,
                    )
                    .as_str()
            ),
            "case {case:?}: expected the sentence that service words for a pipeline the account \
             does not have, got {answered}"
        );
    }
}

// ---------------------------------------------------------------------------
// AWS::CodePipeline::Webhook: the webhook the stack reports really exists
// ---------------------------------------------------------------------------

/// Every webhook `ListWebhooks` reports, as `(definition, url)` pairs, paging through the
/// listing the way a client does.
async fn listed_webhooks(router: &MultiServiceRouter, case: &str) -> Vec<(Value, String)> {
    let mut found = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut request = json!({ "MaxResults": 1 });
        if let Some(token) = &cursor {
            request["NextToken"] = json!(token);
        }
        let (code, listed) = codepipeline(router, "ListWebhooks", &request).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected ListWebhooks to answer, got {code} with {listed}"
        );
        for item in listed["webhooks"].as_array().into_iter().flatten() {
            found.push((
                item["definition"].clone(),
                item["url"].as_str().unwrap_or_default().to_string(),
            ));
        }
        cursor = listed["NextToken"].as_str().map(ToString::to_string);
        if cursor.is_none() {
            return found;
        }
    }
}

/// A webhook a stack declares EXISTS in CodePipeline — `ListWebhooks` reports it with the
/// definition the template declared, and the `Url` the stack publishes is the one that service
/// generated — and deleting the stack takes it with it.
///
/// The rows differ along the authentication the webhook declares (each value of
/// `WebhookAuthenticationType`, with the configuration each one requires) and how many filter
/// rules it carries.
#[tokio::test]
async fn a_declared_webhook_exists_in_codepipeline_and_goes_with_the_stack() {
    // (case, Name, Authentication, the declared AuthenticationConfiguration, the
    // configuration CodePipeline REPORTS for it — a secret token is redacted in that
    // service's own answers ("If a secret token was provided, it will be redacted in the
    // response") — and Filters)
    let rows: [(&str, &str, &str, Value, Value, Value); 3] = [
        (
            "an IP-authenticated webhook with one filter",
            "cpfam-webhook-ip",
            "IP",
            json!({ "AllowedIPRange": "10.0.0.0/8" }),
            json!({ "AllowedIPRange": "10.0.0.0/8" }),
            json!([{ "JsonPath": "$.ref", "MatchEquals": "refs/heads/{Branch}" }]),
        ),
        (
            "an unauthenticated webhook with two filters",
            "cpfam-webhook-open",
            "UNAUTHENTICATED",
            json!({}),
            json!({}),
            json!([
                { "JsonPath": "$.ref", "MatchEquals": "refs/heads/main" },
                { "JsonPath": "$.repository.name" }
            ]),
        ),
        (
            "a GITHUB_HMAC webhook with one filter",
            "cpfam-webhook-hmac",
            "GITHUB_HMAC",
            json!({ "SecretToken": "a-secret-token" }),
            json!({
                "SecretToken":
                    service_com_amazonaws_codepipeline_codepipeline_20150709::state::
                        REDACTED_SECRET_TOKEN
            }),
            json!([{ "JsonPath": "$.action", "MatchEquals": "published" }]),
        ),
    ];
    let router = build_router();

    // The pipeline every row's webhook targets, created through CodePipeline's own API so the
    // rows deploy the webhook alone.
    let case = "the prerequisite pipeline";
    let role_arn = create_role(
        &router,
        case,
        "cpfam-webhook-pipeline-role",
        &assumable_by_codepipeline(),
    )
    .await;
    const TARGET_PIPELINE: &str = "cpfam-webhook-target";
    const TARGET_ACTION: &str = "Fetch";
    let (code, created) = codepipeline(
        &router,
        "CreatePipeline",
        &json!({
            "pipeline": {
                "name": TARGET_PIPELINE,
                "roleArn": role_arn,
                "artifactStore": { "type": "S3", "location": "cpfam-webhook-artifacts" },
                "stages": [
                    {
                        "name": "Source",
                        "actions": [{
                            "name": TARGET_ACTION,
                            "actionTypeId": {
                                "category": "Source", "owner": "AWS",
                                "provider": "S3", "version": "1"
                            },
                            "configuration": {
                                "S3Bucket": "cpfam-webhook-artifacts",
                                "S3ObjectKey": "source.zip",
                                "PollForSourceChanges": "false"
                            },
                            "outputArtifacts": [{ "name": "SourceOut" }],
                            "runOrder": 1
                        }]
                    },
                    {
                        "name": "Deploy",
                        "actions": [{
                            "name": "Ship",
                            "actionTypeId": {
                                "category": "Deploy", "owner": "AWS",
                                "provider": "S3", "version": "1"
                            },
                            "configuration": {
                                "BucketName": "cpfam-webhook-artifacts",
                                "ObjectKey": "shipped.zip",
                                "Extract": "false"
                            },
                            "inputArtifacts": [{ "name": "SourceOut" }],
                            "runOrder": 1
                        }]
                    }
                ]
            }
        }),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected the target pipeline to be created, got {code} with {created}"
    );

    for (case, name, authentication, configuration, reported_configuration, filters) in rows {
        let stack = format!("{name}-stack");
        let template = json!({
            "Resources": {
                "Webhook": {
                    "Type": "AWS::CodePipeline::Webhook",
                    "Properties": {
                        "Name": name,
                        "Authentication": authentication,
                        "AuthenticationConfiguration": configuration,
                        "Filters": filters,
                        "TargetPipeline": TARGET_PIPELINE,
                        "TargetAction": TARGET_ACTION,
                        "TargetPipelineVersion": 1,
                        "RegisterWithThirdParty": false
                    }
                }
            },
            "Outputs": {
                "WebhookRef": { "Value": { "Ref": "Webhook" } },
                "WebhookUrl": { "Value": { "Fn::GetAtt": ["Webhook", "Url"] } }
            }
        })
        .to_string();

        create_stack(&router, case, &stack, &template, "CAPABILITY_IAM").await;
        let (status, described) = settled(&router, case, &stack).await;
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: expected the webhook to be created, got {status:?} with {described}"
        );
        let outputs = resolved_outputs(case, &described);
        assert_eq!(
            output(case, &outputs, "WebhookRef"),
            name,
            "case {case:?}: expected Ref to resolve to the webhook's name, got {outputs:?}"
        );

        // The webhook EXISTS in CodePipeline, with the definition the template declared.
        let listed = listed_webhooks(&router, case).await;
        let (definition, url) = listed
            .iter()
            .find(|(definition, _)| definition["name"].as_str() == Some(name))
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected ListWebhooks to report a webhook named {name:?}, \
                     got {listed:?}"
                )
            });
        assert_eq!(
            definition["targetPipeline"].as_str(),
            Some(TARGET_PIPELINE),
            "case {case:?}: expected the declared target pipeline, got {definition}"
        );
        assert_eq!(
            definition["targetAction"].as_str(),
            Some(TARGET_ACTION),
            "case {case:?}: expected the declared target action, got {definition}"
        );
        assert_eq!(
            definition["authentication"].as_str(),
            Some(authentication),
            "case {case:?}: expected the declared authentication, got {definition}"
        );
        assert_eq!(
            definition["authenticationConfiguration"], reported_configuration,
            "case {case:?}: expected the declared authentication configuration, got {definition}"
        );
        // The filters as `WebhookFilterRule` spells them: one member per declared rule, in
        // the order the declaration lists them.
        let declared_filters: Vec<Value> = filters
            .as_array()
            .into_iter()
            .flatten()
            .map(|rule| {
                let mut spelled = json!({ "jsonPath": rule["JsonPath"] });
                if let Some(matched) = rule.get("MatchEquals") {
                    spelled["matchEquals"] = matched.clone();
                }
                spelled
            })
            .collect();
        assert_eq!(
            definition["filters"],
            json!(declared_filters),
            "case {case:?}: expected the declared filter rules, got {definition}"
        );
        assert_eq!(
            output(case, &outputs, "WebhookUrl"),
            url,
            "case {case:?}: expected the published Url to be the one CodePipeline generated \
             for the webhook, got {outputs:?}"
        );

        // Deleting the stack takes the webhook with it: the account no longer holds it.
        let (code, body) =
            cloudformation(&router, "DeleteStack", &[("StackName", stack.as_str())]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {code} with body {body}"
        );
        let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
        loop {
            let held = listed_webhooks(&router, case).await;
            let still_there = held
                .iter()
                .any(|(definition, _)| definition["name"].as_str() == Some(name));
            if !still_there {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "case {case:?}: expected the webhook to go with the stack, still listed after \
                 {SETTLE_DEADLINE:?}: {held:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}
