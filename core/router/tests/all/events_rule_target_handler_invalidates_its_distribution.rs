// SPDX-License-Identifier: CC0-1.0

//! The "EventBridge rule -> Lambda -> CloudFront invalidation" wiring, driven end to end over
//! the assembled router: a `python3.x` function whose module-scope code imports the AWS SDK
//! and builds a CloudFront client there, handed a matched event by a rule's target, and the
//! invalidation it submitted read back out of CloudFront itself.
//!
//! RULE: a managed `python3.x` runtime puts the AWS SDK on the handler's import path, so an
//! SDK import and a client built at MODULE scope resolve during initialization and the
//! handler's body runs — the synchronous `Invoke` answers HTTP 200 with NO
//! `X-Amz-Function-Error` (`Invoke` binds `FunctionError` to that header, so a module the
//! runtime could not import is reported there instead of a result) and with the handler's own
//! return value as the `Payload`. What that running handler then calls is made against the
//! addressed service under the function's execution role: a `CreateInvalidation` it submits is
//! CloudFront's own record from that moment, reported by `GetInvalidation` and listed by
//! `ListInvalidations` with the batch's own `Paths` and `CallerReference`; a call the role does
//! not permit is refused by CloudFront ITSELF, which the SDK raises as its error class, so the
//! handler's own report is still the payload and nothing is invalidated. None of that depends
//! on WHERE the handler read the distribution it names from: a rule hands its target exactly
//! the document that target asked for (the whole envelope, `Target$Input`, `Target$InputPath`
//! or `Target$InputTransformer`), and an event no rule's pattern matches is handed to nobody.
//!
//! Only a cross-service suite can state this: the rule, the match and the handover are
//! EventBridge's, the function, its runtime and the SDK seam Lambda's, the execution role and
//! the authorization decision IAM's, the stack CloudFormation's, and the invalidation
//! CloudFront's. [`super::lambda_handler_invalidates_its_distribution`] pins that a handler
//! invoked DIRECTLY can submit one, and
//! [`super::s3_object_created_invalidates_the_distribution_edge`] pins an ORIGIN WRITE driving
//! one through a rule whose two targets differ in their resource policy; neither states what a
//! target's INPUT SHAPING hands the handler, nor what the payload of a handler whose CloudFront
//! call its own role does not permit is — the case a client cannot otherwise tell apart from a
//! runtime that could not import the SDK.
//!
//! The harness is [`super::lambda_handler_invalidates_its_distribution`]'s, reused rather than
//! repeated: the router, the request helpers, the origin, the distribution and the stack
//! deployment are that suite's, so the two readings of one wire cannot drift. What is written
//! here is what this suite adds — the EventBridge calls, the rule's target shapings, and the
//! execution-role rows.
//!
//! The first suite's rows differ along what the rule generalises over: the shaping the target
//! asks for, whether the document it is handed names the distribution by its id or by its ARN
//! (which the handler resolves to the id), the form of the paths the batch carries (one exact
//! object, a wildcard over a prefix, several paths, a nested name carrying dots and dashes),
//! and — the negative control — an event whose `detail-type` the rule's pattern does not name.
//! The second suite's rows differ along what the execution role permits: the invalidation
//! actions on the distribution the handler invalidates, the same actions on ANOTHER
//! distribution, and nothing of CloudFront at all.

use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::distribution_arn;
use smithy_server_runtime::MultiServiceRouter;

use crate::lambda_handler_invalidates_its_distribution::{
    authorization, build_router, call, cloudfront, create_distribution, create_origin, deployed,
    wait_until_active, Answer, API_VERSION, LOOK_INTERVAL, REGION,
};
use crate::query_wire::{element, elements};

/// Where Lambda's function routes are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The service principal EventBridge hands a function target over as, and the action a grant
/// for it has to name.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The `detail-type` every rule's pattern names, and the one the negative row publishes
/// instead — which no pattern in this suite names.
const DETAIL_TYPE: &str = "Origin Content Published";
const OTHER_DETAIL_TYPE: &str = "Origin Content Archived";

/// The error code and status CloudFront refuses a call no policy permits with (its
/// `AccessDenied`, HTTP 403 — `smithy_query(query="operation",operation="CreateInvalidation")`).
const ACCESS_DENIED: &str = "AccessDenied";
/// See [`ACCESS_DENIED`].
const ACCESS_DENIED_STATUS: u16 = 403;

/// The IAM action the handler's own call performs.
const CREATE_INVALIDATION_ACTION: &str = "cloudfront:CreateInvalidation";

/// How long a delivery's observable end state is waited for, looked at every
/// [`LOOK_INTERVAL`]. Generous: no row measures the deadline, only the outcome.
const DELIVERY: Duration = Duration::from_secs(60);

/// How long the row that must observe NOTHING keeps looking before concluding nothing came.
const QUIET: Duration = Duration::from_secs(5);

/// The handler every function here runs, with the SDK imported and the client built at MODULE
/// scope: it reads the distribution and the paths out of whatever document it was handed —
/// the envelope's `detail` where it carries one, else the document itself — resolves a
/// distribution named by ARN to the id the API addresses it by, submits the batch, and reports
/// either what CloudFront answered or the refusal the SDK raised for it.
const HANDLER: &str = r#"
import boto3
from botocore.exceptions import ClientError

CLIENT = boto3.client("cloudfront")


def lambda_handler(event, context):
    document = event.get("detail", event)
    distribution = document["distribution"].split("/")[-1]
    try:
        submitted = CLIENT.create_invalidation(
            DistributionId=distribution,
            InvalidationBatch={
                "Paths": {"Items": document["paths"]},
                "CallerReference": document["reference"],
            },
        )
    except ClientError as error:
        return {
            "ok": False,
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "http_status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    invalidation = submitted["Invalidation"]
    return {
        "ok": True,
        "id": invalidation["Id"],
        "state": invalidation["Status"],
        "paths": invalidation["InvalidationBatch"]["Paths"]["Items"],
    }
"#;

/// One Lambda `restJson1` call over the shared request helper.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: &str) -> Answer {
    call(
        router,
        "lambda",
        &format!("lambda.{REGION}.amazonaws.com"),
        method,
        uri,
        "application/json",
        body,
    )
    .await
}

/// The body of a Lambda answer as JSON, or `Null` where there is none.
fn payload_of(answered: &Answer) -> Value {
    if answered.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&answered.body).unwrap_or(Value::Null)
    }
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
///
/// EventBridge speaks an `X-Amz-Target` RPC rather than a path-addressed REST API, so this is
/// the one request the shared helper cannot build.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let host = format!("events.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("authorization", authorization("events"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: EventBridge {operation} must answer 200, got {status} with {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One function the suite deploys: its name, the role it runs as, and the distribution ARN its
/// role grants the invalidation actions on (`None` for a role granted nothing on CloudFront).
struct Deployment {
    function: String,
    role: String,
    granted_on: Option<String>,
}

/// The stack that declares each deployment's execution role and function: every role trusts
/// Lambda, which is what `CreateFunction` checks, and carries the inline policy its deployment
/// names — the document the handler's own CloudFront call is judged against.
fn stack_template(deployments: &[Deployment]) -> String {
    let mut resources = Map::new();
    for (index, deployment) in deployments.iter().enumerate() {
        let role_logical = format!("Role{index}");
        let statements = match &deployment.granted_on {
            Some(arn) => json!([{
                "Effect": "Allow",
                "Action": [
                    "cloudfront:CreateInvalidation",
                    "cloudfront:GetInvalidation",
                    "cloudfront:ListInvalidations",
                ],
                "Resource": arn,
            }]),
            // A role granted nothing of CloudFront still carries the grants every execution
            // role has, so a row differs in the CloudFront grant alone.
            None => json!([{
                "Effect": "Allow",
                "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            }]),
        };
        resources.insert(
            role_logical.clone(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": deployment.role,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "handler-grants",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": statements,
                        },
                    }],
                },
            }),
        );
        resources.insert(
            format!("Function{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": deployment.function,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": { "Fn::GetAtt": [role_logical, "Arn"] },
                    "Timeout": 30,
                    "Code": { "ZipFile": HANDLER },
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Deploy every `deployments` entry in ONE stack and wait until each function is invocable, so
/// the provisioning spans the clock owes run down together rather than one after another.
async fn given_functions(router: &MultiServiceRouter, stack: &str, deployments: &[Deployment]) {
    deployed(router, stack, &stack_template(deployments)).await;
    for deployment in deployments {
        wait_until_active(router, &deployment.function).await;
    }
}

/// The ARN Lambda holds for `name`, read back from the service rather than built here.
async fn function_arn(router: &MultiServiceRouter, name: &str) -> String {
    let answered = lambda(router, "GET", &format!("{FUNCTIONS_URI}/{name}"), "").await;
    let body = payload_of(&answered);
    body["Configuration"]["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: GetFunction({name:?}) reports no FunctionArn: {body}"))
        .to_string()
}

/// The resource-based statement EventBridge's handover of a matched event needs ("for Lambda
/// functions … EventBridge relies on resource-based policies", `PutTargets`).
async fn grant_events_invoke(router: &MultiServiceRouter, name: &str) {
    let granted = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/policy"),
        &json!({
            "StatementId": "events-invoke",
            "Action": INVOKE_ACTION,
            "Principal": EVENTS_PRINCIPAL,
        })
        .to_string(),
    )
    .await;
    assert_eq!(
        granted.status, 201,
        "setup: AddPermission({name:?}) must answer 201, got {} with {}",
        granted.status, granted.body
    );
}

/// The ids CloudFront lists for `distribution`, in the order its own `ListInvalidations`
/// reports them.
async fn invalidation_ids(router: &MultiServiceRouter, distribution: &str) -> Vec<String> {
    let listed = cloudfront(
        router,
        "GET",
        &format!("/{API_VERSION}/distribution/{distribution}/invalidation"),
        "",
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "ListInvalidations({distribution}) must answer 200, got {} with {}",
        listed.status, listed.body
    );
    elements(&listed.body, "Id")
}

/// What `GetInvalidation` reports for one id: the `CallerReference` its batch carried and the
/// paths that batch named.
async fn invalidation_batch(
    router: &MultiServiceRouter,
    distribution: &str,
    id: &str,
) -> (String, Vec<String>) {
    let read = cloudfront(
        router,
        "GET",
        &format!("/{API_VERSION}/distribution/{distribution}/invalidation/{id}"),
        "",
    )
    .await;
    assert_eq!(
        read.status, 200,
        "GetInvalidation({id}) must answer 200, got {} with {}",
        read.status, read.body
    );
    (
        element(&read.body, "CallerReference"),
        elements(&read.body, "Path"),
    )
}

/// Every invalidation CloudFront holds for `distribution`, as the `CallerReference` its batch
/// carried mapped to the paths that batch named — read through the service's own
/// `ListInvalidations` and `GetInvalidation`, which is where an invalidation has to exist.
async fn invalidations(
    router: &MultiServiceRouter,
    distribution: &str,
) -> BTreeMap<String, Vec<String>> {
    let mut held = BTreeMap::new();
    for id in invalidation_ids(router, distribution).await {
        let (reference, paths) = invalidation_batch(router, distribution, &id).await;
        held.insert(reference, paths);
    }
    held
}

/// Look until CloudFront holds an invalidation of `distribution` whose batch carried
/// `reference`, answering the paths it named — or `None` once the deadline passes.
///
/// Each look reads the LIST and then only the ids it has not read before, so a wait does not
/// re-read every invalidation the rows before it left behind.
async fn invalidation_for(
    router: &MultiServiceRouter,
    distribution: &str,
    reference: &str,
    within: Duration,
) -> Option<Vec<String>> {
    let looks = within.as_millis() / LOOK_INTERVAL.as_millis();
    let mut read: BTreeMap<String, String> = BTreeMap::new();
    for _ in 0..=looks {
        for id in invalidation_ids(router, distribution).await {
            if read.contains_key(&id) {
                continue;
            }
            let (carried, paths) = invalidation_batch(router, distribution, &id).await;
            read.insert(id, carried.clone());
            if carried == reference {
                return Some(paths);
            }
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
    None
}

/// What the rule's target asks to be handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shaping {
    /// Nothing: the target receives the whole event envelope.
    Envelope,
    /// A constant document the target carries (`Target$Input`), whatever the event said.
    Constant,
    /// The part of the event a JSON path selects (`Target$InputPath`).
    Selected,
    /// A document rendered from parts of the event (`Target$InputTransformer`).
    Rendered,
}

/// One row of the delivery suite.
struct Row {
    case: &'static str,
    shaping: Shaping,
    /// Whether the document the handler is handed names the distribution by its ARN rather
    /// than by the id the API addresses it by.
    by_arn: bool,
    /// The paths the batch carries, which are what the invalidation has to report back.
    paths: &'static [&'static str],
    /// Whether the event this row publishes carries the `detail-type` the rule's pattern
    /// names: the negative row's does not, so no target is handed anything at all.
    matched: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the whole envelope, whose detail names the distribution by id",
        shaping: Shaping::Envelope,
        by_arn: false,
        paths: &["/assets/app.js"],
        matched: true,
    },
    Row {
        case: "a constant target input naming the distribution by ARN",
        shaping: Shaping::Constant,
        by_arn: true,
        paths: &["/assets/*"],
        matched: true,
    },
    Row {
        case: "the detail selected by the target's input path",
        shaping: Shaping::Selected,
        by_arn: false,
        paths: &["/index.html", "/about-us/index.html"],
        matched: true,
    },
    Row {
        case: "a document the target's input transformer rendered, naming the ARN",
        shaping: Shaping::Rendered,
        by_arn: true,
        paths: &["/media/promo.v2-final.jpg"],
        matched: true,
    },
    Row {
        case: "an event whose detail-type no rule's pattern names",
        shaping: Shaping::Envelope,
        by_arn: false,
        paths: &["/never-invalidated.js"],
        matched: false,
    },
];

/// A rule's function target invalidates the distribution the document it was handed names —
/// whatever shaping that target asked for — and an event no pattern matches invalidates
/// nothing.
#[tokio::test]
async fn a_rules_function_target_invalidates_the_distribution_its_input_names() {
    let router = build_router();
    create_origin(&router).await;
    let distribution = create_distribution(&router, "events-target-invalidation").await;
    let arn = distribution_arn(&distribution);

    let function = "events-target-invalidator".to_string();
    given_functions(
        &router,
        "xsvc-events-target-invalidation",
        &[Deployment {
            function: function.clone(),
            role: "events-target-invalidator-role".to_string(),
            granted_on: Some(arn.clone()),
        }],
    )
    .await;
    let target_arn = function_arn(&router, &function).await;
    grant_events_invoke(&router, &function).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let source = format!("cloudfront.invalidation.row-{index}");
        let rule = format!("invalidation-row-{index}");
        let reference = format!("events-target-row-{index}");
        // What the handler has to end up with, however it was handed to it.
        let named = if row.by_arn {
            arn.clone()
        } else {
            distribution.clone()
        };
        let document = json!({
            "distribution": named,
            "paths": row.paths,
            "reference": reference,
        });

        events(
            &router,
            case,
            "PutRule",
            &json!({
                "Name": rule,
                "EventPattern": json!({
                    "source": [source],
                    "detail-type": [DETAIL_TYPE],
                })
                .to_string(),
                "State": "ENABLED",
            }),
        )
        .await;

        let mut target = json!({ "Id": "invalidator", "Arn": target_arn });
        // The detail the event carries is the one this shaping leaves the handler reading:
        // the constant-input row's event names nothing usable, so a delivery that ignored the
        // target's own input could not invalidate anything.
        let detail = match row.shaping {
            Shaping::Envelope | Shaping::Selected => document.clone(),
            Shaping::Constant => {
                target["Input"] = json!(document.to_string());
                json!({ "published": "the target carries its own document" })
            }
            Shaping::Rendered => {
                target["InputTransformer"] = json!({
                    "InputPathsMap": {
                        "distribution": "$.detail.distribution",
                        "path": "$.detail.path",
                        "reference": "$.detail.reference",
                    },
                    "InputTemplate": "{\"distribution\": \"<distribution>\", \
                                      \"paths\": [\"<path>\"], \"reference\": \"<reference>\"}",
                });
                json!({
                    "distribution": named,
                    "path": row.paths[0],
                    "reference": reference,
                })
            }
        };
        if row.shaping == Shaping::Selected {
            target["InputPath"] = json!("$.detail");
        }

        let wired = events(
            &router,
            case,
            "PutTargets",
            &json!({ "Rule": rule, "Targets": [target] }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {case:?}: the target must be accepted, got {wired}"
        );

        let detail_type = if row.matched {
            DETAIL_TYPE
        } else {
            OTHER_DETAIL_TYPE
        };
        let published = events(
            &router,
            case,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": source,
                    "DetailType": detail_type,
                    "Detail": detail.to_string(),
                }],
            }),
        )
        .await;
        assert_eq!(
            published["FailedEntryCount"],
            json!(0),
            "case {case:?}: the event must be accepted, got {published}"
        );

        // BACKED: the invalidation is CloudFront's own record, read through CloudFront's own
        // API rather than out of anything the handler said.
        let budget = if row.matched { DELIVERY } else { QUIET };
        let held = invalidation_for(&router, &distribution, &reference, budget).await;
        if !row.matched {
            assert_eq!(
                held, None,
                "case {case:?}: no rule's pattern matches this event, so nothing may be \
                 invalidated for it, got {held:?}"
            );
            continue;
        }
        let expected: Vec<String> = row.paths.iter().map(|path| (*path).to_string()).collect();
        assert_eq!(
            held,
            Some(expected.clone()),
            "case {case:?}: expected CloudFront to hold an invalidation of {distribution} \
             carrying {expected:?} for {reference:?}, got {held:?}"
        );
    }
}

/// One row of the execution-role suite: which distribution the role's inline policy grants the
/// invalidation actions on.
struct GrantRow {
    case: &'static str,
    /// `Some(true)` where the policy names the distribution the handler invalidates,
    /// `Some(false)` where it names the OTHER one, and `None` where it names no CloudFront
    /// resource at all.
    scope: Option<bool>,
}

const GRANT_ROWS: &[GrantRow] = &[
    GrantRow {
        case: "a role granted the invalidation on the distribution the handler names",
        scope: Some(true),
    },
    GrantRow {
        case: "a role granted the invalidation on another distribution only",
        scope: Some(false),
    },
    GrantRow {
        case: "a role granted nothing of CloudFront at all",
        scope: None,
    },
];

/// Whatever the execution role permits, a running handler's own report is the payload: the
/// invocation answers 200 with no function error, and the CloudFront call it describes either
/// happened or was refused by CloudFront itself.
#[tokio::test]
async fn a_handlers_own_report_is_the_payload_whatever_its_role_permits() {
    let router = build_router();
    create_origin(&router).await;
    let distribution = create_distribution(&router, "direct-invoke-invalidation").await;
    let elsewhere = create_distribution(&router, "direct-invoke-other").await;
    let arn = distribution_arn(&distribution);
    let other_arn = distribution_arn(&elsewhere);

    let deployments: Vec<Deployment> = GRANT_ROWS
        .iter()
        .enumerate()
        .map(|(index, row)| Deployment {
            function: format!("role-bounded-invalidator-{index}"),
            role: format!("role-bounded-invalidator-role-{index}"),
            granted_on: match row.scope {
                Some(true) => Some(arn.clone()),
                Some(false) => Some(other_arn.clone()),
                None => None,
            },
        })
        .collect();
    given_functions(&router, "xsvc-direct-invoke-invalidation", &deployments).await;

    for (index, row) in GRANT_ROWS.iter().enumerate() {
        let case = row.case;
        let deployment = &deployments[index];
        let reference = format!("direct-invoke-row-{index}");
        let paths = vec![format!("/row-{index}/app.js"), format!("/row-{index}/*")];
        let answered = lambda(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{}/invocations", deployment.function),
            &json!({
                "distribution": distribution,
                "paths": paths,
                "reference": reference,
            })
            .to_string(),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the invocation must be served, got {} with {}",
            answered.status, answered.body
        );
        // The heart of it: a runtime that could not import the SDK reports the run as a fault
        // HERE instead of answering what the handler returned.
        assert_eq!(
            answered.function_error, None,
            "case {case:?}: expected the handler's own result, got a function error with {}",
            answered.body
        );
        let payload = payload_of(&answered);
        assert!(
            payload.get("errorType").is_none() && payload.get("errorMessage").is_none(),
            "case {case:?}: expected the handler's own report, got {payload}"
        );

        if row.scope != Some(true) {
            assert_refused(case, &payload, &deployment.role, &deployment.function, &arn);
            assert_nothing_invalidated(&router, case, &distribution, &elsewhere, &reference).await;
            continue;
        }

        assert_eq!(
            payload["ok"],
            json!(true),
            "case {case:?}: the handler's granted call must report it was made, got {payload}"
        );
        assert_eq!(
            payload["paths"],
            json!(paths),
            "case {case:?}: expected the batch's own paths, got {payload}"
        );
        // An invalidation is ASYNCHRONOUS, so the submit cannot report it finished.
        assert_ne!(
            payload["state"].as_str().unwrap_or_default(),
            "Completed",
            "case {case:?}: an invalidation is asynchronous, so the submit cannot report it \
             finished: {payload}"
        );
        let held = invalidations(&router, &distribution).await;
        assert_eq!(
            held.get(&reference),
            Some(&paths),
            "case {case:?}: expected CloudFront itself to hold the submitted batch, got {held:?}"
        );
    }
}

/// What a refused call has to report: CloudFront's own code, status and sentence, as the SDK
/// raised them for the handler — the refusal naming the session the invocation ran as, the
/// action the handler performed and the distribution it addressed. The sentence is spelled out
/// here as IAM states it rather than read from the emulator, so a change to that wording fails
/// this.
fn assert_refused(case: &str, payload: &Value, role: &str, function: &str, resource: &str) {
    let principal = format!(
        "arn:aws:sts::{}:assumed-role/{role}/{function}",
        engine::emulator_account_id()
    );
    let expected = json!({
        "ok": false,
        "code": ACCESS_DENIED,
        "http_status": ACCESS_DENIED_STATUS,
        "message": format!(
            "User: {principal} is not authorized to perform: {CREATE_INVALIDATION_ACTION} on \
             resource: {resource} because no identity-based policy allows the \
             {CREATE_INVALIDATION_ACTION} action"
        ),
    });
    assert_eq!(
        payload, &expected,
        "case {case:?}: expected the refusal {expected}, got {payload}"
    );
}

/// A refused call invalidated nothing, on either distribution.
async fn assert_nothing_invalidated(
    router: &MultiServiceRouter,
    case: &str,
    distribution: &str,
    elsewhere: &str,
    reference: &str,
) {
    for named in [distribution, elsewhere] {
        let held = invalidations(router, named).await;
        assert!(
            !held.contains_key(reference),
            "case {case:?}: a refused call must invalidate nothing on {named}, got {held:?}"
        );
    }
}
