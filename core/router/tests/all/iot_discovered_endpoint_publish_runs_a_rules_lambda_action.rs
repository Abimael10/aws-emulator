// SPDX-License-Identifier: CC0-1.0

//! The bootstrap sequence every AWS SDK sample prescribes for AWS IoT, followed all the way to
//! the rules engine: ask the control plane WHERE this account's data plane answers
//! (`DescribeEndpoint`), build a data-plane client on the address it published, publish there —
//! and read the invocation the matched topic rule's `lambda` action made.
//!
//! RULE: a discovery operation publishes an address this emulator SERVES the asked-for endpoint
//! type's protocols at, so whatever type a client asks for it is handed a `host[:port]` of this
//! emulator's own and never a host outside it — the message broker's own socket for a data
//! endpoint, which carries MQTT as well as the HTTPS data plane, and the emulator's own endpoint
//! for the credentials provider, which carries HTTPS alone; and a message the data plane accepted
//! at that address is evaluated by the rules engine exactly as one accepted at any other address
//! of the same emulator is, so each matched rule's `lambda` action invokes the function its
//! `functionArn` names — as the IoT service principal, and therefore only where that FUNCTION's
//! own resource-based policy admits `lambda:InvokeFunction` for the invoking rule's ARN.
//!
//! Three crates have to agree for that to hold — the IoT control plane answers the discovery and
//! holds the rules, the IoT data plane takes the publish addressed at the published value, and
//! Lambda holds the function's resource policy and records the event it accepted — so the suite
//! lives here: a crate-local suite can only ever see one side of it.
//!
//! What this suite is about that its neighbours are not: it is the COMPOSITION of the two, over a
//! serving base that MOVES. The published value on its own is pinned, across serving bases and
//! endpoint types, by the IoT crate's own `describe_endpoint.rs`, and a discovered address
//! serving a plane by its `endpoint_discovery_reachability.rs`; the rules engine's `lambda`
//! action reached by a CloudFormation-deployed rule and its declared permission by
//! `cfn_deployed_iot_topic_rule_invokes_its_lambda.rs` beside this file; and a discovery
//! followed by a publish whose rule forwards to a QUEUE by
//! `iot_discovered_data_endpoint_routes_a_published_message.rs`. What none of them does is move
//! the address the emulator is served at and require the `lambda` leg — discovery, publish and
//! the permission boundary — to follow it, which is the one path a client that only ever learns
//! its endpoint from `DescribeEndpoint` takes.
//!
//! The rows differ along what the rule generalises over: the ADDRESS this emulator is served at,
//! which is what the published value has to be and what the publish is then addressed to; WHICH
//! endpoint type the client discovered the plane with — every type
//! `DescribeEndpointRequest$endpointType` documents that AWS still resolves an endpoint for, plus
//! the request naming none, which asks for the default; and whether the rule's target function
//! admits the invocation at all. Each row's message carries its own marker, so no row can pass by
//! reading another's.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::query_wire::form_encode;

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The addresses this emulator is served at, one row each, as the core publishes one from its
/// data port: two loopback binds on different ports, so no single value satisfies both rows, and
/// an IPv6 bind, whose authority keeps the brackets its own colons live in. Each row stands up
/// its own emulator on its own engine at that base.
const SERVING_BASES: &[&str] = &[
    "https://127.0.0.1:39357",
    "https://127.0.0.1:41821",
    "https://[::1]:8443",
];

/// Where Lambda binds the collection the functions are created, read and permissioned under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A deployment package as `Code.ZipFile` carries it: base64 in a `restJson1` JSON body. Nothing
/// here asserts what a handler computed — what is pinned is the event the function was handed —
/// so the package holds no code at all.
const ZIP_FILE: &str = "UEsDBAoAAAAAAAAAAAA=";

/// The runtime, handler, execution role and timeout both functions carry: all well-formed for
/// that model's own members, none of them what a row is about.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";
const FUNCTION_TIMEOUT: u64 = 30;

/// The `State` a function has to report before an invocation of it is accepted.
const ACTIVE: &str = "Active";

/// The IAM action an invocation of a function performs, and the principal AWS IoT invokes a
/// rule's function as.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";
const IOT_PRINCIPAL: &str = "iot.amazonaws.com";

/// The two legs: one whose function's resource policy admits the invocation its rule makes, one
/// whose function carries no policy at all. A rule name carries letters, digits and underscores
/// alone (`RuleName`, `pattern ^[a-zA-Z0-9_]+$`).
const GRANTED_RULE: &str = "iot_discovered_granted";
const DENIED_RULE: &str = "iot_discovered_denied";
const GRANTED_TOPIC: &str = "iotdisc/granted";
const DENIED_TOPIC: &str = "iotdisc/denied";
const GRANTED_FUNCTION: &str = "iot-discovered-granted";
const DENIED_FUNCTION: &str = "iot-discovered-denied";

/// How long an observation of work the CLOCK owes — a function going `Active`, the record of an
/// accepted invocation appearing — waits before calling it a hang, and how long between two
/// looks. Generous: a row fails only when the work did not happen at all, so a loaded host
/// cannot decide the outcome, and bounded, so an exhausted wait cannot make the suite slow.
const DEADLINE: Duration = Duration::from_secs(60);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// One discovery row: the `endpointType` the client asks for, and the label its two messages are
/// marked with.
///
/// The catalog is the `endpointType` member's own documentation — "Valid endpoint types include:
/// `iot:Data` ... `iot:Data-ATS` ... `iot:CredentialProvider` ... `iot:Jobs`", read with
/// `smithy_query(query="doc",shape="DescribeEndpointRequest",member="endpointType",
/// model_path="services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json")`
/// — minus `iot:Jobs`, which AWS resolves no endpoint for any more and which is therefore
/// refused rather than served (driven in the IoT crate's own `describe_endpoint.rs`), plus the
/// request that names no type at all.
const DISCOVERY_ROWS: &[(Option<&str>, &str)] = &[
    (None, "no-endpoint-type"),
    (Some("iot:Data"), "iot-data"),
    (Some("iot:Data-ATS"), "iot-data-ats"),
    (Some("iot:CredentialProvider"), "iot-credential-provider"),
];

/// The ARN of one function, which is what a rule's `lambda` action names.
fn function_arn(function: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{function}")
}

/// The ARN of one topic rule, which is the `aws:SourceArn` a grant for its actions is narrowed
/// by.
fn rule_arn(rule: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:rule/{rule}")
}

/// The marker the message published to the granted leg for `label` carries, and the one the
/// denied leg's carries.
fn granted_marker(label: &str) -> String {
    format!("granted-{label}")
}

/// See [`granted_marker`].
fn denied_marker(label: &str) -> String {
    format!("denied-{label}")
}

/// The `host[:port]` of a serving base: the address a client reaches the emulator at, which is
/// what an address it is HANDED has to be, since what discovery publishes is a value a path is
/// appended to and not a URL.
///
/// Derived from the base the router was configured with rather than written beside it, so the
/// expectation and the configuration cannot drift apart.
fn authority_of(base: &EndpointUrl) -> String {
    let url = base.as_str();
    url.split_once("://")
        .map(|(_, authority)| authority)
        .unwrap_or(url)
        .to_string()
}

/// The address the endpoint type `endpoint_type` is SERVED at on the emulator behind `engine`,
/// which is what discovery has to publish for it.
///
/// A DATA endpoint is the account's message-broker address — the host AWS serves MQTT over TLS
/// and the HTTPS data-plane API at — so this emulator publishes the socket it opened for that
/// broker (`service_common::iot_mqtt_broker`, reached through the data plane that reports on its
/// sessions); every other type carries HTTPS alone and is published at the emulator's own
/// endpoint, `control`.
fn served_at(
    engine: &Arc<::engine::InMemoryEngine>,
    endpoint_type: Option<&str>,
    control: &str,
) -> String {
    let data_endpoint = matches!(endpoint_type, None | Some("iot:Data") | Some("iot:Data-ATS"));
    if !data_endpoint {
        return control.to_string();
    }
    service_com_amazonaws_iotdataplane_iotmoonrakerservice::state::iot_mqtt_broker::published_broker_endpoint(&**engine)
        .expect("discovering a data endpoint opens the broker's socket")
}

/// The whole emulator over one engine, exactly as the binary assembles it, served at `base` as
/// the core serves it from its data port — with the engine kept, so the record Lambda holds of
/// the invocations it accepted can be read, and the authority a client reaches it by.
fn emulator(base: &str) -> (MultiServiceRouter, Arc<::engine::InMemoryEngine>, String) {
    let base = EndpointUrl::parse(base).expect("the served base is a usable endpoint");
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router.set_url_base(base.clone());
    let authority = authority_of(&base);
    (router, engine, authority)
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One restJson1 call into the emulator, ADDRESSED at `authority` — the host a client built on a
/// published address sends, which for every data-plane call below is the discovered value itself.
async fn rest(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    authority: &str,
    path: &str,
    body: Option<Vec<u8>>,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{authority}{path}"))
        .header("host", authority)
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/json")
        .body(Bytes::from(body.unwrap_or_default()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body().to_vec();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `GET /endpoint?endpointType=…` on the control plane, answered with the address it published.
async fn discover(
    router: &MultiServiceRouter,
    case: &str,
    control: &str,
    endpoint_type: Option<&str>,
) -> String {
    let path = match endpoint_type {
        Some(requested) => format!("/endpoint?endpointType={}", form_encode(requested)),
        None => "/endpoint".to_string(),
    };
    let (status, answered) = rest(router, "iot", "GET", control, &path, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeEndpoint must answer 200, got {status} with {answered}"
    );
    answered
        .get("endpointAddress")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no endpointAddress in {answered}"))
        .to_string()
}

/// Create one function carrying no code, which is all a row that asserts the EVENT it was handed
/// needs.
async fn create_function(router: &MultiServiceRouter, control: &str, function: &str) {
    let (status, answered) = rest(
        router,
        "lambda",
        "POST",
        control,
        FUNCTIONS_URI,
        Some(
            json!({
                "FunctionName": function,
                "Role": ROLE,
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Timeout": FUNCTION_TIMEOUT,
                "Code": { "ZipFile": ZIP_FILE },
            })
            .to_string()
            .into_bytes(),
        ),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: CreateFunction({function}) must succeed, got {status} with {answered}"
    );
}

/// Look again until `function` reports `Active`, which is the state "you can't invoke or modify
/// the function" before. Waits on the CONDITION, not on a span.
async fn wait_for_active(router: &MultiServiceRouter, control: &str, function: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let (status, configuration) = rest(
            router,
            "lambda",
            "GET",
            control,
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: GetFunctionConfiguration({function}) must succeed, got {status} with \
             {configuration}"
        );
        if configuration["State"] == json!(ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("setup: {function} never reported {ACTIVE}, last {last}");
}

/// Grant the IoT service principal the invocation of `function` for `rule`'s ARN, as
/// `AddPermission` writes one statement of a function's resource policy.
async fn add_permission(
    router: &MultiServiceRouter,
    control: &str,
    function: &str,
    rule: &str,
) {
    let (status, answered) = rest(
        router,
        "lambda",
        "POST",
        control,
        &format!("{FUNCTIONS_URI}/{function}/policy"),
        Some(
            json!({
                "StatementId": rule,
                "Action": INVOKE_ACTION,
                "Principal": IOT_PRINCIPAL,
                "SourceArn": rule_arn(rule),
            })
            .to_string()
            .into_bytes(),
        ),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: AddPermission({function}, {rule}) must succeed, got {status} with {answered}"
    );
}

/// Store one topic rule whose single action invokes `function` for every message published to
/// `topic`, as `CreateTopicRule` takes it: the body IS the `topicRulePayload`.
async fn create_topic_rule(
    router: &MultiServiceRouter,
    control: &str,
    rule: &str,
    topic: &str,
    function: &str,
) {
    let (status, answered) = rest(
        router,
        "iot",
        "POST",
        control,
        &format!("/rules/{rule}"),
        Some(
            json!({
                "sql": format!("SELECT * FROM '{topic}'"),
                "awsIotSqlVersion": "2016-03-23",
                "ruleDisabled": false,
                "actions": [{ "lambda": { "functionArn": function_arn(function) } }],
            })
            .to_string()
            .into_bytes(),
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateTopicRule({rule}) must succeed, got {status} with {answered}"
    );
}

/// Publish one marked message to `topic` AT `authority` — the address discovery published — which
/// is what the rules engine runs on. An accepted publish is answered 200 whatever the rules did
/// with it, which is why every assertion below is made on the TARGET.
///
/// The topic travels as ONE path segment: a `{topic}` label carries no `/` of its own, so a
/// topic name's separators are escaped by this crate's one encoder of an unreserved-set
/// component ([`crate::query_wire::form_encode`]) — which is how an SDK sends one.
async fn publish(
    router: &MultiServiceRouter,
    case: &str,
    authority: &str,
    topic: &str,
    marker: &str,
) {
    let (status, answered) = rest(
        router,
        "iotdata",
        "POST",
        authority,
        &format!("/topics/{}?qos=1", form_encode(topic)),
        Some(json!({ "marker": marker }).to_string().into_bytes()),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: Publish({topic}) at the discovered address {authority} must be accepted, \
         got {status} with {answered}"
    );
}

/// The event document of every asynchronous invocation Lambda has ACCEPTED for `function`,
/// oldest first — the owning service's own record of what it was handed.
fn accepted(engine: &Arc<::engine::InMemoryEngine>, function: &str) -> Vec<Value> {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let store = state.lock();
    store
        .functions
        .get(function)
        .map(|record| {
            record
                .async_invocations
                .iter()
                .map(|entry| {
                    serde_json::from_slice(&entry.payload).unwrap_or_else(|error| {
                        panic!(
                            "the event handed to {function} must be a JSON document ({error}): \
                             {:?}",
                            String::from_utf8_lossy(&entry.payload)
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Look again until Lambda holds `expected` accepted invocations for `function`, and report them.
///
/// The record exists by the time the publish that caused it is answered — the rules engine makes
/// the invocation on that request's own path — so this is a guard against a future change rather
/// than a wait the suite depends on: it waits on the CONDITION either way, and reports what
/// Lambda held when it gave up.
async fn accepted_once_settled(
    engine: &Arc<::engine::InMemoryEngine>,
    function: &str,
    expected: usize,
) -> Vec<Value> {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let held = accepted(engine, function);
        if held.len() >= expected || Instant::now() >= giving_up_at {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One serving base: stand the whole emulator up there, then discover and publish through the
/// address it publishes, for every discovery row.
async fn an_estate_served_at(base: &str) {
    let (router, engine, control) = emulator(base);

    for function in [GRANTED_FUNCTION, DENIED_FUNCTION] {
        create_function(&router, &control, function).await;
    }
    for function in [GRANTED_FUNCTION, DENIED_FUNCTION] {
        wait_for_active(&router, &control, function).await;
    }
    // The whole of the authorization gate: the granted leg's function admits the invocation its
    // own rule makes, the denied leg's function carries no resource policy at all.
    add_permission(&router, &control, GRANTED_FUNCTION, GRANTED_RULE).await;

    create_topic_rule(
        &router,
        &control,
        GRANTED_RULE,
        GRANTED_TOPIC,
        GRANTED_FUNCTION,
    )
    .await;
    create_topic_rule(
        &router,
        &control,
        DENIED_RULE,
        DENIED_TOPIC,
        DENIED_FUNCTION,
    )
    .await;

    for (endpoint_type, label) in DISCOVERY_ROWS {
        let case = format!("{base} / {}", endpoint_type.unwrap_or("no endpointType"));
        let address = discover(&router, &case, &control, *endpoint_type).await;

        // The published value is the address this emulator SERVES that endpoint type at: the
        // broker's own socket for a data endpoint, which carries the MQTT a device connects with
        // beside this HTTPS plane, and the emulator's own endpoint for the credentials provider.
        // Either way it is an address a client of this emulator can DIAL — and never one of AWS's
        // own hosts, which resolves off this machine and reaches no plane of this emulator at all.
        let expected = served_at(&engine, *endpoint_type, &control);
        assert_eq!(
            address, expected,
            "case {case:?}: expected the address this emulator serves that endpoint type at, got \
             {address:?}"
        );

        // The second leg, made exactly as a client that only ever learns its endpoint from
        // discovery makes it: addressed at the value it was handed.
        publish(
            &router,
            &case,
            &address,
            DENIED_TOPIC,
            &denied_marker(label),
        )
        .await;
        publish(
            &router,
            &case,
            &address,
            GRANTED_TOPIC,
            &granted_marker(label),
        )
        .await;
    }

    // One invocation per matched message, carrying that message: the rule the publish matched
    // ran its `lambda` action for every endpoint type the client discovered the plane with.
    let expected: BTreeSet<String> = DISCOVERY_ROWS
        .iter()
        .map(|(_, label)| granted_marker(label))
        .collect();
    let events = accepted_once_settled(&engine, GRANTED_FUNCTION, expected.len()).await;
    let markers: BTreeSet<String> = events
        .iter()
        .filter_map(|event| event.get("marker")?.as_str().map(str::to_string))
        .collect();
    assert_eq!(
        markers, expected,
        "case {base:?}: expected the rule's lambda action to be handed exactly these messages, \
         got {markers:?} in {events:?}"
    );
    assert_eq!(
        events.len(),
        expected.len(),
        "case {base:?}: expected one invocation per matched message, {} in all, got {events:?}",
        expected.len()
    );

    // The leg whose function admits nothing was invoked for no message at all, judged after
    // every expected invocation has been recorded — so a silent target is a refused invocation
    // and not an undispatched one.
    let held = accepted(&engine, DENIED_FUNCTION);
    assert!(
        held.is_empty(),
        "case {base:?}: expected the rules engine to invoke the target carrying no resource \
         policy for no message at all, got {held:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_publish_at_the_discovered_endpoint_runs_the_matched_rules_permitted_lambda_action() {
    for base in SERVING_BASES {
        an_estate_served_at(base).await;
    }
}
