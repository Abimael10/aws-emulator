// SPDX-License-Identifier: CC0-1.0

//! What a handler that PROBES ITS OWN PERMISSIONS across several services answers, end to end
//! over the wire: Lambda, Amazon EventBridge, Amazon SQS, AWS Step Functions, IAM and the token
//! service on ONE engine, a bare `.zip` function with no layer whose module does `import boto3`,
//! and every side effect it makes read back where a client reads it.
//!
//! RULE: a managed `python3.x` runtime supplies the AWS SDK on the handler's import path and
//! exports the Region and the function role's temporary credentials into the handler environment
//! — so `import boto3` at module scope resolves with no layer deployed, a client built with
//! neither a Region nor credentials of its own calls as a session of the function's execution
//! role, and `Invoke` answers HTTP 200 with NO `X-Amz-Function-Error` and the handler's own
//! returned document as the payload (`Invoke` binds `FunctionError` to the
//! `X-Amz-Function-Error` header and `Payload` to the body,
//! `smithy_query(query="operation", operation="Invoke")`). A handler written to report a verdict
//! per service therefore answers ITS OWN keys: the service's own result where the policies in
//! force on the execution role allow that call, and that service's own access-denied code,
//! status and IAM sentence where they do not.
//!
//! What this rules out is the shape the defect had: a module whose SDK import did not resolve
//! never runs its body, so the invocation answers `X-Amz-Function-Error: Unhandled` with a
//! `Runtime.ImportModuleError` document carrying none of the handler's own keys, and no side
//! effect the handler would have made happens at all — an allow-vs-deny probe then cannot tell
//! "permitted" from "denied", because both arms come back as the same import error.
//!
//! The rows differ along what the rule generalises over: WHICH managed point release of the
//! runtime runs the module, WHICH service of the three the probed call reaches (an `awsJson1_1`
//! publication, an `awsJson1_0` send and an `awsJson1_0` execution start — three protocols and
//! three access-denied vocabularies), and WHETHER the policies in force on the execution role
//! allow that call. The role's one inline document is rewritten BETWEEN invocations, so each row
//! also pins that the document in force when the handler calls is the one that decides. Every row
//! asserts both halves: the verdict the handler reported, and what the BUS, the QUEUE and the
//! STATE MACHINE hold afterwards — so a probe that answered "allowed" without publishing,
//! sending or starting anything fails here.
//!
//! What the neighbours pin and this suite does not repeat: `python_runtime_bundled_sdk` drives
//! the import FORMS and the point releases against one service, `handler_step_functions_sdk`
//! drives the operations of one service a handler reaches, and `handler_eventbridge_publication`
//! drives the policy shapes a publication is judged under without running a handler. What only
//! this one reaches is a single RUNNING handler's own verdict document over three services at
//! once, which is what a least-privilege probe deployed as a bare `.zip` answers.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at most
//! `max_concurrent_runs` invocations at once for the whole process.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role, iam, logs_only};
use crate::event_source_mappings::{await_function_active, call, call_sqs, create_queue, router_on};
// The queue drain and the sentence IAM words an implicit deny with belong to the suite that first
// needed them, reached here rather than copied: the wording of a refusal has one owner.
use crate::handler_queue_and_secret_grant_scoping::{drained, implicit_deny};
use crate::handler_s3_object_access::session_arn;
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::synchronous_invoke_run::package;

/// The Region every request below is signed for, and the one each invocation — and so the
/// clients its handler builds — runs in.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and attribute each package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The execution role every function of this suite runs as, and the name of the ONE inline
/// identity policy the rows rewrite on it — one name, so each row replaces the document the row
/// before it left rather than adding a second one.
const ROLE_NAME: &str = "sdk-permission-probe-role";
const INLINE_POLICY: &str = "probe-grants";

/// The bus the probed publication names: the account's default one, which exists without being
/// created.
const BUS: &str = "default";

/// The rule on that bus which carries this suite's own control queue, its target id, and the
/// source and detail type its pattern matches — so a message on that queue is one of this
/// suite's publications and nothing else.
const RULE: &str = "receives-probe-publications";
const TARGET_ID: &str = "t1";
const SOURCE: &str = "com.demo.permission-probe";
const DETAIL_TYPE: &str = "Permission Probed";

/// The service principal EventBridge delivers to a queue target as, which is the principal the
/// target queue's own policy admits.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The two queues: the one the probed `SendMessage` addresses, and the one the rule above
/// delivers the probed publication to.
const PROBE_QUEUE: &str = "permission-probe-sends";
const RULE_QUEUE: &str = "permission-probe-publications";

/// The state machine the probed `StartExecution` names, and the role it is created with.
const MACHINE_NAME: &str = "permission-probe-machine";
const MACHINE_ROLE: &str = "arn:aws:iam::000000000000:role/StepFunctionsRole";

/// A definition that succeeds at once with a static result, so an execution the probe starts is
/// one the service really carries.
const MACHINE_DEFINITION: &str =
    r#"{"StartAt":"Hello","States":{"Hello":{"Type":"Pass","Result":{"msg":"hi"},"End":true}}}"#;

/// The execution input the probed `StartExecution` carries.
const EXECUTION_INPUT: &str = r#"{"from":"the probe"}"#;

/// The one word a handler reports for a call its role let through, which is what a
/// least-privilege probe's own verdict document carries.
const ALLOWED: &str = "ALLOWED";

/// The three reserved variables a managed runtime exports the invocation's credential in, in the
/// order a credential's parts are named (the Lambda Developer Guide's reserved runtime
/// environment variables). The handler reports only THAT each is set, never a value.
const CREDENTIAL_VARIABLES: [&str; 3] = [
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
];

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is what the probe answered and not how fast an
/// interpreter gets through it.
const FUNCTION_TIMEOUT: i64 = 120;

/// What each service answers a call its policies do not allow with, and the status it carries:
/// EventBridge and Step Functions answer the AWS-common `AccessDeniedException`, each in its own
/// JSON envelope at the status that endpoint carries every client error at, and Amazon SQS the
/// AWS-common `AccessDenied` at HTTP 403.
const EVENTS_DENIED: &str = "AccessDeniedException";
const EVENTS_DENIED_STATUS: i64 = 400;
const QUEUE_DENIED: &str = "AccessDenied";
const QUEUE_DENIED_STATUS: i64 = 403;
const STATES_DENIED: &str = "AccessDeniedException";
const STATES_DENIED_STATUS: i64 = 400;

/// The actions the three probed calls are judged under, as each service's own refusal NAMES them:
/// Amazon SQS words a refusal with the action case-folded, the other two with the action as the
/// model spells the operation.
const EVENTS_ACTION: &str = "events:PutEvents";
const QUEUE_ACTION: &str = "sqs:sendmessage";
const STATES_ACTION: &str = "states:StartExecution";

/// The handler every function of this suite runs.
///
/// It imports the SDK and builds all three clients at MODULE scope — with neither a Region nor
/// credentials of its own, so what they resolve comes from the runtime environment — then makes
/// exactly one call per service and reports a verdict for each: [`ALLOWED`], or the code, status
/// and sentence the service refused it with. The payload of the invocation IS this document, so a
/// row reads the probe's own verdict rather than anything the emulator decided for it.
const PROBE_SOURCE: &str = r#"
import json
import os

import boto3
import botocore.exceptions

EVENTS = boto3.client("events")
QUEUES = boto3.client("sqs")
WORKFLOWS = boto3.client("stepfunctions")

CREDENTIAL_VARIABLES = [
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
]

ALLOWED = "ALLOWED"


def refusal(error):
    return {
        "code": error.response["Error"]["Code"],
        "message": error.response["Error"]["Message"],
        "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
    }


def verdict(call):
    try:
        call()
    except botocore.exceptions.ClientError as error:
        return refusal(error)
    return ALLOWED


def published(probe):
    entry = {
        "EventBusName": probe["bus"],
        "Source": probe["source"],
        "DetailType": probe["detail_type"],
        "Detail": json.dumps({"marker": probe["marker"]}),
    }
    try:
        answered = EVENTS.put_events(Entries=[entry])
    except botocore.exceptions.ClientError as error:
        return refusal(error)
    if answered.get("FailedEntryCount"):
        failed = answered["Entries"][0]
        return {
            "code": failed.get("ErrorCode"),
            "message": failed.get("ErrorMessage"),
            "status": answered["ResponseMetadata"]["HTTPStatusCode"],
        }
    return ALLOWED


def lambda_handler(event, context):
    probe = event["probe"]
    return {
        "region": os.environ.get("AWS_REGION"),
        # The credential itself stays inside the run: what is reported is only THAT the runtime
        # exported each part of it, which is what the clients above signed with.
        "credential_parts": sorted(
            name for name in CREDENTIAL_VARIABLES if os.environ.get(name)
        ),
        "events": published(probe),
        "sqs": verdict(
            lambda: QUEUES.send_message(
                QueueUrl=probe["queue_url"], MessageBody=probe["marker"]
            )
        ),
        "stepfunctions": verdict(
            lambda: WORKFLOWS.start_execution(
                stateMachineArn=probe["state_machine_arn"],
                name=probe["execution_name"],
                input=probe["execution_input"],
            )
        ),
    }
"#;

/// Lambda over the SAME engine as the three services the probe calls, plus the IAM store the
/// execution role lives in and the token service its session is minted by — exactly what
/// `core/router/src/lib.rs` assembles. A handler's own SDK call is dispatched back into this same
/// router, so the bus, the queue and the state machine it reaches are the ones this suite reads.
fn probe_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// One `awsJson` RPC call against the service whose signing name is `signing_name` — the
/// credential scope is what routes it — made with the credential this harness publishes, which no
/// store holds and so speaks for the account itself.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEMULATOR/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&payload).to_string()))
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must be served.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Step Functions call (`awsJson1_0`, `AWSStepFunctions.<Operation>`) that must be served.
async fn states(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        router,
        case,
        "states",
        "AWSStepFunctions",
        "1.0",
        operation,
        body,
    )
    .await
}

/// The ARN of the event bus `name` in the Region every call here is signed for — the resource a
/// statement about a publication onto it names, and the one a refusal of that publication states.
fn bus_arn(name: &str) -> String {
    format!(
        "arn:aws:events:{REGION}:{}:event-bus/{name}",
        engine::emulator_account_id()
    )
}

/// The identity policy in force on the execution role from now on: `statements` as its one inline
/// document, written with IAM's own operation.
async fn given_policy(router: &MultiServiceRouter, case: &str, statements: &[Value]) {
    let document = json!({"Version": "2012-10-17", "Statement": statements}).to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", document.as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected PutRolePolicy to answer 200, got {status} {written}"
    );
}

/// Create the queue the rule below targets, give it the policy EventBridge's own delivery needs,
/// and put the rule that watches the default bus for this suite's own event on it. Answers the
/// queue's url.
async fn given_rule_queue(router: &MultiServiceRouter, case: &str) -> String {
    let (url, arn) = create_queue(router, case, RULE_QUEUE, &json!({})).await;
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridge",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    })
    .to_string();
    let (status, set) = call_sqs(
        router,
        "SetQueueAttributes",
        &json!({"QueueUrl": url, "Attributes": {"Policy": policy}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected SetQueueAttributes to answer 200, got {status} {set}"
    );
    let pattern = json!({"source": [SOURCE], "detail-type": [DETAIL_TYPE]}).to_string();
    events(
        router,
        case,
        "PutRule",
        &json!({"Name": RULE, "EventPattern": pattern, "State": "ENABLED"}),
    )
    .await;
    let targets = events(
        router,
        case,
        "PutTargets",
        &json!({"Rule": RULE, "Targets": [{"Id": TARGET_ID, "Arn": arn}]}),
    )
    .await;
    assert_eq!(
        targets["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {targets}"
    );
    url
}

/// The `detail.marker` of every event waiting on the rule's queue, taken off it and sorted.
async fn delivered_markers(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<String> {
    let mut markers: Vec<String> = drained(router, case, url, wanted)
        .await
        .iter()
        .map(|text| {
            let envelope: Value = serde_json::from_str(text).unwrap_or(Value::Null);
            assert_eq!(
                envelope["source"].as_str().unwrap_or_default(),
                SOURCE,
                "case {case:?}: expected this suite's own source on the rule's queue, got \
                 {envelope}"
            );
            envelope["detail"]["marker"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    markers.sort();
    markers
}

/// The names of every execution of the state machine `machine_arn`, as Step Functions reports
/// them, every page of the listing followed so a name this suite looks for cannot be missed for
/// sitting past the first page.
async fn execution_names(router: &MultiServiceRouter, case: &str, machine_arn: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut request = json!({"stateMachineArn": machine_arn});
        if let Some(token) = &next {
            request["nextToken"] = json!(token);
        }
        let listed = states(router, case, "ListExecutions", &request).await;
        names.extend(
            listed["executions"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|execution| execution["name"].as_str().map(str::to_string)),
        );
        next = listed["nextToken"].as_str().map(str::to_string);
        if next.is_none() {
            return names;
        }
    }
}

/// The names of [`CREDENTIAL_VARIABLES`] sorted, which is how the handler reports the parts of
/// the credential its environment carried.
fn exported_credential_parts() -> Vec<&'static str> {
    let mut names = CREDENTIAL_VARIABLES.to_vec();
    names.sort_unstable();
    names
}

/// One row: the function it invokes — which is to say the point release that runs the module, each
/// function of [`FUNCTIONS`] being one — and whether the role's one inline document allows the
/// three probed calls.
struct Row {
    case: &'static str,
    function: &'static str,
    permitted: bool,
}

/// The rows, in the order they run: one point release under a role that allows each probed call,
/// the same release under a role that allows none of them, and another point release under a role
/// that allows each again — so what is pinned is the runtime FAMILY and the policy in force at the
/// instant of the call, not one release and one document.
const ROWS: &[Row] = &[
    Row {
        case: "python3.12 under a role granting each probed action",
        function: EARLIER_FUNCTION,
        permitted: true,
    },
    Row {
        case: "python3.12 under a role granting none of the probed actions",
        function: EARLIER_FUNCTION,
        permitted: false,
    },
    Row {
        case: "python3.13 under a role granting each probed action",
        function: LATER_FUNCTION,
        permitted: true,
    },
];

/// The two functions, one per point release, and the releases they run.
const EARLIER_FUNCTION: &str = "permission-probe-312";
const LATER_FUNCTION: &str = "permission-probe-313";

/// The deployments the rows invoke: created once each, and the two rows on the same release invoke
/// the same function so the release is the axis and not the deployment.
const FUNCTIONS: &[(&str, &str)] = &[
    (EARLIER_FUNCTION, "python3.12"),
    (LATER_FUNCTION, "python3.13"),
];

/// Create the function `name` on `runtime`, running as `role_arn` and carrying the probe above as
/// a bare one-file `.zip` with no layer.
async fn probe_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    runtime: &str,
    role_arn: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": runtime,
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(PROBE_SOURCE)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, case, name).await;
}

/// Assert that one verdict of the probe's document is the service's own access-denied refusal,
/// naming the session the call was made as, the action and the resource the request named.
fn assert_denied(
    case: &str,
    service: &str,
    verdict: &Value,
    code: &str,
    status: i64,
    expected: &str,
) {
    assert_eq!(
        verdict["code"],
        json!(code),
        "case {case:?}: expected the {service} verdict to carry that service's own \
         authorization code, got {verdict}"
    );
    assert_eq!(
        verdict["status"],
        json!(status),
        "case {case:?}: expected the status that service answers a refusal with for {service}, \
         got {verdict}"
    );
    assert_eq!(
        verdict["message"],
        json!(expected),
        "case {case:?}: expected the refusal {expected:?} for {service}, got {verdict}"
    );
}

/// RULE: a managed `python3.x` runtime supplies the AWS SDK on the handler's import path and
/// exports the Region and the function role's temporary credentials into the handler environment,
/// so a bare `.zip` handler that does `import boto3` runs and `Invoke` answers HTTP 200 with no
/// `X-Amz-Function-Error` and the handler's OWN verdict document — the service's result where the
/// policies in force on the execution role allow the call, that service's own access-denied code,
/// status and IAM sentence where they do not — and each served call really happened, read back
/// where a client reads it.
#[tokio::test]
async fn a_python_handler_probes_its_own_permissions_with_the_sdk_the_runtime_supplies() {
    let _stream = one_invocation_stream_at_a_time().await;
    let setup = "the role, the bus rule, the queues and the state machine of the probe table";
    let router = probe_router();

    let (probe_queue_url, probe_queue_arn) =
        create_queue(&router, setup, PROBE_QUEUE, &json!({})).await;
    let rule_queue_url = given_rule_queue(&router, setup).await;
    let created = states(
        &router,
        setup,
        "CreateStateMachine",
        &json!({
            "name": MACHINE_NAME,
            "definition": MACHINE_DEFINITION,
            "roleArn": MACHINE_ROLE,
        }),
    )
    .await;
    let machine_arn = created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {setup:?}: expected a stateMachineArn, got {created}"))
        .to_string();

    // The role starts out carrying nothing but the log writes; each row writes the document it is
    // about before it invokes.
    let role_arn = given_role(&router, setup, ROLE_NAME, Some(json!([logs_only()]))).await;

    // The grant each permitted row runs under: the three probed actions, each scoped by `Resource`
    // to the one resource its call names.
    let permitting = vec![
        logs_only(),
        allow("events", &["PutEvents"], &bus_arn(BUS)),
        allow("sqs", &["SendMessage"], &probe_queue_arn),
        allow("states", &["StartExecution"], &machine_arn),
    ];
    // And the grant each refusing row runs under: ANOTHER action of each of the three services, so
    // the role still holds grants on all three and what refuses the probed calls is an implicit
    // deny about THEIR action rather than a role that holds nothing at all.
    let refusing = vec![
        logs_only(),
        allow("events", &["PutRule"], "*"),
        allow("sqs", &["ReceiveMessage"], "*"),
        allow("states", &["DescribeStateMachine"], "*"),
    ];

    for (name, runtime) in FUNCTIONS {
        probe_function(&router, setup, name, runtime, &role_arn).await;
    }

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        given_policy(
            &router,
            case,
            if row.permitted {
                &permitting
            } else {
                &refusing
            },
        )
        .await;

        let marker = format!("probe-row-{index}");
        let execution_name = format!("started-by-row-{index}");
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{}/invocations", row.function),
            &json!({"probe": {
                "bus": BUS,
                "source": SOURCE,
                "detail_type": DETAIL_TYPE,
                "marker": marker,
                "queue_url": probe_queue_url,
                "state_machine_arn": machine_arn,
                "execution_name": execution_name,
                "execution_input": EXECUTION_INPUT,
            }}),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
            answered.status,
            answered.text()
        );
        // The defect this suite was written for: a module whose SDK import did not resolve is
        // reported HERE, and its payload carries none of the handler's own keys.
        let signal = answered.header("x-amz-function-error");
        let payload = answered.json();
        assert_eq!(
            signal, None,
            "case {case:?}: the handler must run to completion, got the function error \
             {signal:?} with {payload}"
        );

        // The payload IS the handler's own verdict document: its three per-service keys are there
        // whichever way each call was answered.
        for service in ["events", "sqs", "stepfunctions"] {
            assert!(
                !payload[service].is_null(),
                "case {case:?}: expected the probe's own {service:?} verdict in the payload, got \
                 {payload}"
            );
        }
        // And the SDK resolved the Region and the credential out of the runtime environment, which
        // is what let a client built with neither of them call at all.
        assert_eq!(
            payload["region"],
            json!(REGION),
            "case {case:?}: expected the runtime to export the invocation's Region, got {payload}"
        );
        assert_eq!(
            payload["credential_parts"],
            json!(exported_credential_parts()),
            "case {case:?}: expected the runtime to export every part of the role's temporary \
             credential, got {payload}"
        );

        let principal = session_arn(ROLE_NAME, row.function);
        if row.permitted {
            for service in ["events", "sqs", "stepfunctions"] {
                assert_eq!(
                    payload[service],
                    json!(ALLOWED),
                    "case {case:?}: expected the probe's {service:?} verdict to be {ALLOWED:?}, \
                     got {payload}"
                );
            }
        } else {
            assert_denied(
                case,
                "events",
                &payload["events"],
                EVENTS_DENIED,
                EVENTS_DENIED_STATUS,
                &implicit_deny(&principal, EVENTS_ACTION, &bus_arn(BUS)),
            );
            assert_denied(
                case,
                "sqs",
                &payload["sqs"],
                QUEUE_DENIED,
                QUEUE_DENIED_STATUS,
                &implicit_deny(&principal, QUEUE_ACTION, &probe_queue_arn),
            );
            assert_denied(
                case,
                "stepfunctions",
                &payload["stepfunctions"],
                STATES_DENIED,
                STATES_DENIED_STATUS,
                &implicit_deny(&principal, STATES_ACTION, &machine_arn),
            );
        }

        // What the three services hold afterwards, each read where a client reads it: a verdict of
        // ALLOWED means the call really happened, and a refused one left nothing behind.
        let wanted = usize::from(row.permitted);
        let expected_markers: Vec<String> = if row.permitted {
            vec![marker.clone()]
        } else {
            Vec::new()
        };
        let delivered = delivered_markers(&router, case, &rule_queue_url, wanted).await;
        assert_eq!(
            delivered, expected_markers,
            "case {case:?}: expected the markers {expected_markers:?} on the rule's queue, got \
             {delivered:?}"
        );
        let sent = drained(&router, case, &probe_queue_url, wanted).await;
        assert_eq!(
            sent, expected_markers,
            "case {case:?}: expected the bodies {expected_markers:?} on the probed queue, got \
             {sent:?}"
        );
        let names = execution_names(&router, case, &machine_arn).await;
        assert_eq!(
            names.contains(&execution_name),
            row.permitted,
            "case {case:?}: expected the state machine to carry the execution \
             {execution_name:?} only where the probe was allowed to start it, got {names:?}"
        );
    }
}
