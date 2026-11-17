// SPDX-License-Identifier: CC0-1.0

//! A gate AppConfig consults before it starts a deployment decides what is IN SERVICE: a
//! refused `StartDeployment` leaves the configuration the last completed deployment put in
//! service exactly as it was and takes no deployment number, and the SAME request, once a
//! gate lets it through, is accepted and puts the candidate version's bytes in service.
//!
//! RULE: an extension action point AppConfig consults synchronously BEFORE performing a call
//! gates that call, so an action whose target cannot be invoked refuses it with
//! `BadRequestException` / HTTP 400 (`smithy_query(query="operation",
//! operation="StartDeployment")`) and persists nothing — the environment's deployment sequence
//! and the bytes its data plane serves stay the ones the refused call never got to change —
//! while a call whose gate's target accepts the event is accepted, takes the NEXT deployment
//! number of that environment, and the version it names becomes the one served.
//!
//! Only a cross-service suite can state that end to end: the gate's `Uri` names a resource of
//! another service — a Lambda function for the gate that cannot be invoked, and an Amazon SQS
//! queue for the one that takes the event and answers nothing — AppConfig holds the gate and
//! the deployment sequence, and what is in service is read where a CLIENT reads it, out of
//! AppConfig's own data plane, never out of the control-plane answer being checked.
//!
//! What the neighbours pin and this one does not repeat: the AppConfig crate's
//! `extension_gate_refusal_message` pins the wording of the refusal per class of target
//! failure, `extension_action_points` the event a gate is handed at each binding level, and
//! `deployment_tick_directive` what a tick's directive makes of a rollout. None of them follows
//! a refused release through to the deployment sequence and the bytes a client is served next.
//!
//! The rows differ along what the rule generalises over: how many consecutive refusals precede
//! the accepted call (so the accepted one takes the next number of the sequence rather than one
//! per attempt), the media type and bytes of the configuration (a JSON document, a text
//! document, and bytes that are not text at all), and which of the three levels an extension
//! association binds the gate at.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for, and so the Region the queue, the function ARN
/// and the AppConfig hierarchy belong to.
const REGION: &str = "us-east-1";

/// The `LocationUri` that selects AppConfig's own hosted configuration store.
const HOSTED: &str = "hosted";

/// The media type a JSON body travels with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The fixed text between what the invocation reported and the id of the invocation that
/// reported it, in the refusal AppConfig states for a failed extension action. A target
/// Lambda never ran has no invocation of its own to name, so no refusal here carries it.
const REQUEST_ID_MARKER: &str = ", Lambda Request ID: ";

/// The `Type` a `PRE_START_DEPLOYMENT` event names its action point by: the action point in
/// PascalCase, not the SCREAMING_SNAKE key the `Actions` map is keyed by.
const PRE_START_TYPE: &str = "PreStartDeployment";

/// The `DeploymentState` values a deployment can no longer leave
/// (`smithy_query(query="shape", shape="DeploymentState")`), which a row's seeding and
/// accepted deployments both end in.
const TERMINAL_STATES: &[&str] = &["COMPLETE", "ROLLED_BACK", "REVERTED"];

/// How often work the clock owes is looked for again — a rollout reaching its targets, a
/// deployment settling, an event reaching the gate's target — and how long the looking goes on.
///
/// Those spans belong to the service that owns them, so nothing here sleeps a fixed span on its
/// behalf: every wait is on the CONDITION. Each deadline is a ceiling on the WAIT and nothing
/// else — generous enough that a loaded host cannot decide any outcome, because this suite runs
/// beside every other test of the workspace on the same machine, and short enough that a
/// genuine regression is reported rather than waited out several times over.
const LOOK: Duration = Duration::from_millis(50);
const SETTLED_BY: Duration = Duration::from_secs(120);
const OBSERVED_BY: Duration = Duration::from_secs(60);

/// Which of the three levels an extension association binds the gate at
/// (`smithy_query(query="doc", shape="CreateExtensionAssociationRequest",
/// member="ResourceIdentifier")`: "an AppConfig application, environment, or configuration
/// profile"). Every one of them binds the gate of a deployment into that environment.
#[derive(Clone, Copy, Debug)]
enum Level {
    Application,
    Environment,
    Profile,
}

/// One row: what the gate does to a release, and the release it does it to.
#[derive(Debug)]
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// How many times the gate that cannot be invoked is asked before one that takes the event
    /// is bound.
    refusals: usize,
    /// The media type both versions of this row are written with.
    content_type: &'static str,
    /// The bytes of the candidate version the refused calls name.
    candidate: &'static [u8],
    /// The bytes already in service when the refusals start, written as version 1.
    in_service: &'static [u8],
    /// Where the gate's association is bound.
    level: Level,
}

/// The rows. Each one drives the same request through a gate that refuses it and then through
/// one that lets it through.
const ROWS: &[Row] = &[
    Row {
        case: "one refusal, a JSON document, the gate bound to the environment",
        refusals: 1,
        content_type: JSON_CONTENT_TYPE,
        candidate: br#"{"release":"second","rollout":"canary"}"#,
        in_service: br#"{"release":"first","rollout":"none"}"#,
        level: Level::Environment,
    },
    Row {
        case: "two refusals, a text document, the gate bound to the application",
        refusals: 2,
        content_type: "text/plain",
        candidate: b"second release\nflags: on\n",
        in_service: b"first release\nflags: off\n",
        level: Level::Application,
    },
    Row {
        case: "one refusal, bytes that are not text, the gate bound to the profile",
        refusals: 1,
        content_type: "application/octet-stream",
        candidate: &[0x00, 0x01, 0xff, 0xfe, 0x7f, 0x80],
        in_service: &[0xff, 0x00, 0x10, 0x20],
        level: Level::Profile,
    },
];

/// The emulator as it is ASSEMBLED, on one engine: an extension action's invocation is
/// dispatched back into this same router, so the resource its `Uri` names is a record of this
/// emulator's own and the configuration a client polls is the one AppConfig holds.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, without a computed signature.
    router.set_request_authentication(false);
    router
}

/// What one dispatched request answered.
struct Answered {
    status: u16,
    headers: ::http::HeaderMap,
    body: Bytes,
}

impl Answered {
    /// The body as JSON, or `Value::Null` for an empty body.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&self.body).to_string()))
    }

    /// The body as text, for a failure message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// One string member of a JSON body.
    fn string(&self, member: &str) -> Option<String> {
        self.json()
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// One response header, as text.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The wire error code of an error response.
    fn error_code(&self) -> String {
        self.string("__type")
            .or_else(|| self.header("x-amzn-errortype"))
            .unwrap_or_else(|| self.text())
    }
}

/// One call to make: the endpoint host it is addressed at, the credential scope that routes it
/// to the service claiming it, and how that service's protocol carries it.
struct Call<'a> {
    service: &'a str,
    signing_name: &'a str,
    method: &'a str,
    uri: &'a str,
    content_type: &'a str,
    /// The `X-Amz-Target` an RPC protocol names its operation in; `None` for a REST protocol,
    /// which names it by method and path.
    target: Option<&'a str>,
}

/// Dispatch one call with `body` as its request body.
async fn dispatch(router: &MultiServiceRouter, call: &Call<'_>, body: &[u8]) -> Answered {
    let host = format!("{}.{REGION}.amazonaws.com", call.service);
    let mut request = ::http::Request::builder()
        .method(call.method)
        .uri(format!("https://{host}{}", call.uri))
        .header("host", host.clone())
        .header("authorization", authorization(call.signing_name))
        .header("content-type", call.content_type);
    if let Some(target) = call.target {
        request = request.header("x-amz-target", target);
    }
    let request = request
        .body(Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One AppConfig CONTROL-plane call carrying a JSON body.
async fn appconfig(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    appconfig_payload(router, method, uri, JSON_CONTENT_TYPE, &payload).await
}

/// One AppConfig CONTROL-plane call whose body is the configuration's own bytes, of its own
/// media type — the form `CreateHostedConfigurationVersion` binds its `@httpPayload` content
/// and its `@required` `Content-Type` header in.
async fn appconfig_payload(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    content_type: &str,
    body: &[u8],
) -> Answered {
    dispatch(
        router,
        &Call {
            service: "appconfig",
            signing_name: "appconfig",
            method,
            uri,
            content_type,
            target: None,
        },
        body,
    )
    .await
}

/// One AppConfig DATA-plane call, as a polling client makes one. Both of AppConfig's APIs sign
/// as `appconfig`, whichever endpoint a client addresses.
async fn appconfigdata(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    dispatch(
        router,
        &Call {
            service: "appconfigdata",
            signing_name: "appconfig",
            method,
            uri,
            content_type: JSON_CONTENT_TYPE,
            target: None,
        },
        &payload,
    )
    .await
}

/// One Amazon SQS `awsJson1_0` RPC.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let target = format!("AmazonSQS.{operation}");
    let answered = dispatch(
        router,
        &Call {
            service: "sqs",
            signing_name: "sqs",
            method: "POST",
            uri: "/",
            content_type: "application/x-amz-json-1.0",
            target: Some(&target),
        },
        body.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "SQS {operation} must succeed, got {} with {}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// A queue for a gate's `Uri` to name; answers `(url, arn)`.
///
/// A queue is the target kind that takes an action point's event and ANSWERS NOTHING, so it can
/// neither veto the call nor steer it — which is what makes it the gate that lets a release
/// through, and the place the event handed to the action point is observable as a client sees
/// it.
async fn given_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs(router, "CreateQueue", &json!({"QueueName": name})).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateQueue({name:?}) answered no QueueUrl: {created}"))
        .to_string();
    let attributes = sqs(
        router,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the queue {name:?} reports no QueueArn: {attributes}"))
        .to_string();
    (url, arn)
}

/// Every event waiting on the queue, parsed, oldest first, each DELETED by its receipt handle
/// as it is read — so what a later read answers is what arrived since, and no message can be
/// counted twice whatever the queue's visibility window does.
async fn drain(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut events = Vec::with_capacity(messages.len());
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
        let body = message["Body"].as_str().unwrap_or_default();
        events.push(serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("an action-point event must be a JSON document ({error}): {body:?}")
        }));
    }
    events
}

/// How many reads in a row must come back empty before the queue is taken to hold no further
/// event, once an event of the wanted type has arrived.
///
/// An assertion about HOW MANY events a call handed its gate can be passed by timing if the
/// reading stops at the first one, so the reading goes on until the queue has been quiet for
/// this many consecutive reads — which is what makes a second, later dispatch count against the
/// assertion rather than go unseen.
const QUIET_READS: usize = 3;

/// How many times the configuration in service is read again after a refused release, each read
/// asserting it is still the one the last completed deployment put there.
///
/// A release that was wrongly persisted and rolled out behind the refusal would reach the data
/// plane a moment later, which one read taken straight after the 400 can miss; these are the
/// looks that catch it.
const LOOKS_AFTER_A_REFUSAL: usize = 3;

/// Every event the gate's target was handed, read again until at least one of `wanted` has
/// arrived and the queue has gone quiet, or the deadline passes.
///
/// One `ReceiveMessage` is not promised to answer every message waiting on a queue, so this wait
/// is on the CONDITION like every other one here, and each message read is deleted, so nothing
/// is counted twice.
async fn handed_events(router: &MultiServiceRouter, url: &str, wanted: &str) -> Vec<Value> {
    let deadline = Instant::now() + OBSERVED_BY;
    let mut collected: Vec<Value> = Vec::new();
    let mut quiet = 0_usize;
    loop {
        let read = drain(router, url).await;
        quiet = if read.is_empty() { quiet + 1 } else { 0 };
        collected.extend(read);
        let arrived = collected.iter().any(|event| event["Type"] == json!(wanted));
        if (arrived && quiet >= QUIET_READS) || Instant::now() >= deadline {
            return collected;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// One minted id out of a 201 answer.
fn minted_id(case: &str, what: &str, answered: &Answered) -> String {
    assert_eq!(
        answered.status,
        201,
        "case {case:?}: {what} must be created, got {} with {}",
        answered.status,
        answered.text()
    );
    answered
        .string("Id")
        .unwrap_or_else(|| panic!("case {case:?}: {what} answered no Id: {}", answered.text()))
}

/// An extension binding ONE action at `PRE_START_DEPLOYMENT`, pointing at `uri`. Answers its
/// `(id, name)` — the name being what a refusal of a gated call names it by.
async fn given_gate_extension(
    router: &MultiServiceRouter,
    name: &str,
    uri: &str,
) -> (String, String) {
    let created = appconfig(
        router,
        "POST",
        "/extensions",
        &json!({
            "Name": name,
            "Actions": {
                "PRE_START_DEPLOYMENT": [{"Name": format!("{name}-action"), "Uri": uri}],
            },
        }),
    )
    .await;
    (
        minted_id("the gate extensions", name, &created),
        name.to_string(),
    )
}

/// A deployment strategy that reaches every target in one step and bakes for no time, so a
/// deployment of it is over when its start answers and no row waits on another's rollout.
async fn given_strategy(router: &MultiServiceRouter, name: &str) -> String {
    let created = appconfig(
        router,
        "POST",
        "/deploymentstrategies",
        &json!({
            "Name": name,
            "DeploymentDurationInMinutes": 0,
            "FinalBakeTimeInMinutes": 0,
            "GrowthFactor": 100,
            "GrowthType": "LINEAR",
            "ReplicateTo": "NONE",
        }),
    )
    .await;
    minted_id(
        "the deployment strategy",
        "the deployment strategy",
        &created,
    )
}

/// The ARN of the resource a row's association binds the gate to.
fn bound_resource(level: Level, application: &str, environment: &str, profile: &str) -> String {
    let resource = match level {
        Level::Application => format!("application/{application}"),
        Level::Environment => format!("application/{application}/environment/{environment}"),
        Level::Profile => format!("application/{application}/configurationprofile/{profile}"),
    };
    engine::emulator_arn("appconfig", REGION, &resource)
}

/// Bind `extension` to `resource_arn`; answers the association's id.
async fn associate(
    router: &MultiServiceRouter,
    case: &str,
    extension: &str,
    resource_arn: &str,
) -> String {
    let created = appconfig(
        router,
        "POST",
        "/extensionassociations",
        &json!({"ExtensionIdentifier": extension, "ResourceIdentifier": resource_arn}),
    )
    .await;
    minted_id(case, "the extension association", &created)
}

/// Unbind one association, so the gate it bound is no longer consulted.
async fn disassociate(router: &MultiServiceRouter, case: &str, association: &str) {
    let removed = appconfig(
        router,
        "DELETE",
        &format!("/extensionassociations/{association}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        removed.status,
        204,
        "case {case:?}: DeleteExtensionAssociation must succeed, got {} with {}",
        removed.status,
        removed.text()
    );
}

/// The deployment numbers one environment holds, as `ListDeployments` reports them.
async fn deployment_numbers(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
) -> Vec<i64> {
    let listed = appconfig(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "case {case:?}: ListDeployments must succeed, got {} with {}",
        listed.status,
        listed.text()
    );
    let mut numbers: Vec<i64> = listed.json()["Items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item["DeploymentNumber"].as_i64())
        .collect();
    numbers.sort_unstable();
    numbers
}

/// Read one deployment until it reaches a state it can no longer leave, and answer the document
/// it was last reported with. The wait is on the CONDITION.
async fn settled_deployment(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    number: i64,
) -> Value {
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let read = appconfig(
            router,
            "GET",
            &format!("/applications/{application}/environments/{environment}/deployments/{number}"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDeployment({number}) must succeed, got {} with {}",
            read.status,
            read.text()
        );
        let document = read.json();
        let state = document["State"].as_str().unwrap_or_default().to_string();
        if TERMINAL_STATES.contains(&state.as_str()) {
            return document;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: deployment {number} never reached a terminal state, it is still \
             {state:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The configuration AppConfig's data plane serves ONE client right now, as its bytes: a
/// session of its own, then the configuration that session is handed.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
) -> Vec<u8> {
    let session = appconfigdata(
        router,
        "POST",
        "/configurationsessions",
        &json!({
            "ApplicationIdentifier": application,
            "EnvironmentIdentifier": environment,
            "ConfigurationProfileIdentifier": profile,
        }),
    )
    .await;
    assert_eq!(
        session.status,
        201,
        "case {case:?}: StartConfigurationSession must succeed, got {} with {}",
        session.status,
        session.text()
    );
    let token = session
        .string("InitialConfigurationToken")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: StartConfigurationSession answered no token: {}",
                session.text()
            )
        });
    let read = appconfigdata(
        router,
        "GET",
        &format!("/configuration?configuration_token={}", form_encode(&token)),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetLatestConfiguration must succeed, got {} with {}",
        read.status,
        read.text()
    );
    read.body.to_vec()
}

/// What a client is served once the rollout the deployment reported terminal has reached it:
/// read again until the bytes are `expected` or the deadline passes, then answer the last read,
/// which is what the caller asserts on.
async fn serving(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    environment: &str,
    profile: &str,
    expected: &[u8],
) -> Vec<u8> {
    let deadline = Instant::now() + OBSERVED_BY;
    loop {
        let bytes = served(router, case, application, environment, profile).await;
        if bytes == expected || Instant::now() >= deadline {
            return bytes;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// Bytes as a failure message reports them: the text when they are text, so a row whose
/// document is readable fails readably, and the bytes themselves otherwise.
fn shown(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => format!("{text:?}"),
        Err(_) => format!("{bytes:?}"),
    }
}

/// RULE: a gate that refuses a release changes nothing a client can see — the deployment
/// sequence and the configuration in service are the ones the refused call never reached — and
/// the identical request, once a gate lets it through, takes the next deployment number and
/// puts its version in service.
#[tokio::test]
async fn a_refused_release_changes_nothing_and_the_same_request_lands_once_a_gate_admits_it() {
    let router = build_router();
    // The gate that cannot be invoked: a function ARN nothing created, which Lambda itself
    // refuses the invocation of — the invocation failure AppConfig refuses the gated call for.
    let absent_function = engine::emulator_arn("lambda", REGION, "function:nobody-created-me");
    let (queue_url, queue_arn) = given_queue(&router, "release-gate-observations").await;
    let (refusing_extension, refusing_name) =
        given_gate_extension(&router, "release-gate-refuses", &absent_function).await;
    let (admitting_extension, _) =
        given_gate_extension(&router, "release-gate-admits", &queue_arn).await;
    let strategy = given_strategy(&router, "release-gate-instant").await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = index + 1;
        let application = minted_id(
            case,
            "the application",
            &appconfig(
                &router,
                "POST",
                "/applications",
                &json!({"Name": format!("gated-app-{suffix}")}),
            )
            .await,
        );
        let environment = minted_id(
            case,
            "the environment",
            &appconfig(
                &router,
                "POST",
                &format!("/applications/{application}/environments"),
                &json!({"Name": format!("gated-env-{suffix}")}),
            )
            .await,
        );
        let profile = minted_id(
            case,
            "the configuration profile",
            &appconfig(
                &router,
                "POST",
                &format!("/applications/{application}/configurationprofiles"),
                &json!({
                    "Name": format!("gated-profile-{suffix}"),
                    "LocationUri": HOSTED,
                }),
            )
            .await,
        );

        // Version 1, the one a refused release has to leave in service, and version 2, the
        // candidate every attempt below names.
        for (number, content) in [(1, row.in_service), (2, row.candidate)] {
            let written = appconfig_payload(
                &router,
                "POST",
                &format!(
                    "/applications/{application}/configurationprofiles/{profile}\
                     /hostedconfigurationversions"
                ),
                row.content_type,
                content,
            )
            .await;
            assert_eq!(
                written.status,
                201,
                "case {case:?}: writing version {number} must succeed, got {} with {}",
                written.status,
                written.text()
            );
        }

        // Version 1 into service, with no gate bound yet.
        let seeding = appconfig(
            &router,
            "POST",
            &format!("/applications/{application}/environments/{environment}/deployments"),
            &json!({
                "DeploymentStrategyId": strategy,
                "ConfigurationProfileId": profile,
                "ConfigurationVersion": "1",
            }),
        )
        .await;
        assert_eq!(
            seeding.status,
            201,
            "case {case:?}: the seeding deployment must be accepted, got {} with {}",
            seeding.status,
            seeding.text()
        );
        let seeded = settled_deployment(&router, case, &application, &environment, 1).await;
        assert_eq!(
            seeded["State"],
            json!("COMPLETE"),
            "case {case:?}: the seeding deployment must complete, got {seeded}"
        );
        let before = serving(
            &router,
            case,
            &application,
            &environment,
            &profile,
            row.in_service,
        )
        .await;
        assert_eq!(
            before,
            row.in_service,
            "case {case:?}: expected version 1 in service before the gated release, read again \
             until OBSERVED_BY passed, expected {} got {}",
            shown(row.in_service),
            shown(&before)
        );

        // The gate that cannot be invoked, bound at this row's level.
        let resource = bound_resource(row.level, &application, &environment, &profile);
        let refusing_association = associate(&router, case, &refusing_extension, &resource).await;

        // The ONE request every attempt of this row makes, byte for byte.
        let release = json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": "2",
        });
        let deployments =
            format!("/applications/{application}/environments/{environment}/deployments");
        let opening = format!("Error invoking extension {refusing_name}: ");
        for attempt in 1..=row.refusals {
            let refused = appconfig(&router, "POST", &deployments, &release).await;
            assert_eq!(
                refused.status,
                400,
                "case {case:?}: attempt {attempt} must be refused with the modelled 400, got {} \
                 with {}",
                refused.status,
                refused.text()
            );
            assert_eq!(
                refused.error_code().split('#').next_back(),
                Some("BadRequestException"),
                "case {case:?}: attempt {attempt} must be refused as BadRequestException, got {}",
                refused.error_code()
            );
            let message = refused.string("Message").unwrap_or_default();
            assert!(
                message.starts_with(&opening),
                "case {case:?}: attempt {attempt} expected a refusal naming the extension whose \
                 action failed, opening {opening:?}, got {message:?}"
            );
            // And relaying what the target's own service reported about the invocation it
            // refused: the resource it could not resolve, which is the `Uri` the action names.
            let reported = format!("Function not found: {absent_function}");
            assert!(
                message.contains(&reported),
                "case {case:?}: attempt {attempt} expected the refusal to relay {reported:?}, \
                 got {message:?}"
            );
            assert!(
                !message.contains(REQUEST_ID_MARKER),
                "case {case:?}: attempt {attempt} invoked no run of the target's own, so the \
                 refusal has no invocation to name, got {message:?}"
            );

            // Nothing a client can see changed: no record was added to the environment's
            // deployment sequence, and the configuration in service is still version 1's.
            let numbers = deployment_numbers(&router, case, &application, &environment).await;
            assert_eq!(
                numbers,
                vec![1],
                "case {case:?}: attempt {attempt} must leave the deployment sequence alone, got \
                 {numbers:?}"
            );
            // Read again, several times over, rather than once: a release that was wrongly
            // persisted and rolled out behind the 400 would reach the data plane a moment
            // later, and a single read taken straight after the refusal could miss it.
            for look in 1..=LOOKS_AFTER_A_REFUSAL {
                if look > 1 {
                    tokio::time::sleep(LOOK).await;
                }
                let still = served(&router, case, &application, &environment, &profile).await;
                assert_eq!(
                    still,
                    row.in_service,
                    "case {case:?}: attempt {attempt} must leave version 1 in service, read \
                     {look} expected {} got {}",
                    shown(row.in_service),
                    shown(&still)
                );
            }
        }

        // The gate changes: the association whose target cannot be invoked goes, one whose
        // target takes the event takes its place at the SAME resource, and the identical
        // request is made again.
        disassociate(&router, case, &refusing_association).await;
        associate(&router, case, &admitting_extension, &resource).await;

        let accepted = appconfig(&router, "POST", &deployments, &release).await;
        assert_eq!(
            accepted.status,
            201,
            "case {case:?}: the admitted release must be accepted, got {} with {}",
            accepted.status,
            accepted.text()
        );
        // The refused attempts took no number, so this one is the SECOND deployment of the
        // environment however many times the gate refused it.
        assert_eq!(
            accepted.json()["DeploymentNumber"],
            json!(2),
            "case {case:?}: the admitted release takes the next deployment number, got {}",
            accepted.json()
        );
        let numbers = deployment_numbers(&router, case, &application, &environment).await;
        assert_eq!(
            numbers,
            vec![1, 2],
            "case {case:?}: expected the seeding deployment and the admitted one, got {numbers:?}"
        );
        let landed = settled_deployment(&router, case, &application, &environment, 2).await;
        assert_eq!(
            landed["State"],
            json!("COMPLETE"),
            "case {case:?}: the admitted release must complete, got {landed}"
        );
        assert_eq!(
            landed["ConfigurationVersion"],
            json!("2"),
            "case {case:?}: the admitted release deploys the version it named, got {landed}"
        );

        // The gate really was consulted for the call it let through: its target holds the ONE
        // event of this deployment's action point, naming the resources the deployment runs on,
        // the candidate's own bytes and its version as a STRING.
        let events = handed_events(&router, &queue_url, PRE_START_TYPE).await;
        let handed: Vec<&Value> = events
            .iter()
            .filter(|event| event["Type"] == json!(PRE_START_TYPE))
            .collect();
        assert_eq!(
            handed.len(),
            1,
            "case {case:?}: the admitted release must consult its gate once, read until the \
             queue went quiet or OBSERVED_BY passed, got {events:?}"
        );
        let event = handed[0];
        assert_eq!(
            (
                event["Application"]["Id"].as_str(),
                event["Environment"]["Id"].as_str(),
                event["ConfigurationProfile"]["Id"].as_str(),
                event["DeploymentNumber"].as_i64(),
                event["ContentVersion"].as_str(),
            ),
            (
                Some(application.as_str()),
                Some(environment.as_str()),
                Some(profile.as_str()),
                Some(2),
                Some("2"),
            ),
            "case {case:?}: expected the event of this deployment, got {event}"
        );
        let carried = ::aws_smithy_types::base64::decode(
            event["Content"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: Content must be base64: {event}")),
        )
        .unwrap_or_else(|error| {
            panic!("case {case:?}: Content must be base64 ({error}): {event}")
        });
        assert_eq!(
            carried,
            row.candidate,
            "case {case:?}: expected the candidate version's bytes in the event, expected {} got \
             {}",
            shown(row.candidate),
            shown(&carried)
        );

        // And the data plane follows the deployment: the candidate version's exact bytes are
        // served.
        let after = serving(
            &router,
            case,
            &application,
            &environment,
            &profile,
            row.candidate,
        )
        .await;
        assert_eq!(
            after,
            row.candidate,
            "case {case:?}: expected the candidate version's bytes in service, read again until \
             OBSERVED_BY passed, expected {} got {}",
            shown(row.candidate),
            shown(&after)
        );
    }
}
