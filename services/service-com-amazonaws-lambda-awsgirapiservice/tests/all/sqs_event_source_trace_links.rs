// SPDX-License-Identifier: CC0-1.0

//! What AWS X-Ray holds for a batch an event source mapping on an Amazon SQS queue delivered to
//! a `TracingConfig` `Active` function: the trace of the invocation, and what it says about the
//! chains the delivered messages belonged to.
//!
//! The rule under test, in one sentence: **a delivery an event source mapping's poller makes
//! for messages that carry their own tracing context runs under a chain of its OWN — never one
//! of theirs, because one batch may carry messages of many chains — and the segment X-Ray
//! records for Lambda's handling of that invocation REFERS back to each distinct chain the
//! batch's messages belonged to, as one `links` entry naming that chain and
//! `aws.xray.reserved.reference_type` `parent`; a batch whose messages belong to no chain X-Ray
//! reads refers back to none.**
//!
//! Every row drives real HTTP requests through the production `MultiServiceRouter` with AWS
//! Lambda, Amazon SQS and AWS X-Ray on ONE engine, as `core/router` assembles them: the queue
//! is created and written through Amazon SQS's own surface (`SendMessage` with the
//! `AWSTraceHeader` system attribute — "currently, the only supported message system attribute
//! is AWSTraceHeader"), the function carries a REAL deployment package, and the trace is read
//! back the way a client reads one — `GetTraceSummaries` with a `service("<function name>")`
//! filter expression over the delivery window, then `BatchGetTraces` on the ids it answered. So
//! a link no invocation published, or a trace that merely continued the message's chain instead
//! of referring to it, fails here.
//!
//! The rows differ along what the rule generalises over: how many messages the delivery carries,
//! how many distinct chains they belong to, whether two messages repeat ONE chain, whether the
//! batch they are delivered in is one batch or one per message, and whether a message carries a
//! tracing context at all (none, and one naming no chain X-Ray reads).
//!
//! Nothing here waits a span of its own choosing: the poll passes are this suite's own
//! (`crate::event_source_mappings::DrivenPolling`), and every state a row needs is waited for by
//! looking again until it holds, so no assertion depends on how fast the host runs.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_function_active, call, call_sqs, create_queue, given_enabled_mapping, queue_held,
    router_on, DrivenPolling, LOOK_AGAIN,
};

/// Where `CreateFunction` is bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for, and the one the ARNs carry.
const REGION: &str = "us-east-1";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The system attribute a message carries its own tracing context in: "currently, the only
/// supported message system attribute is AWSTraceHeader. Its type must be String and its value
/// must be a correctly formatted X-Ray trace header string"
/// (`SendMessageRequest$MessageSystemAttributes`).
const TRACE_ATTRIBUTE: &str = "AWSTraceHeader";

/// The attribute of a `links` entry that says how the linked trace relates to the segment, and
/// the value a link to the chain the work was ASKED for by carries.
const REFERENCE_TYPE_ATTRIBUTE: &str = "aws.xray.reserved.reference_type";
/// See [`REFERENCE_TYPE_ATTRIBUTE`].
const PARENT_REFERENCE: &str = "parent";

/// Two chains a caller may have named, each a well-formed X-Ray trace id (`1-<8 hex>-<24 hex>`),
/// and the span a tracer names beside one.
const FIRST_CHAIN: &str = "1-5759e988-bd862e3fe1be46a994272793";
/// See [`FIRST_CHAIN`].
const SECOND_CHAIN: &str = "1-6af1c255-0000000000000000aaaabbbb";
/// See [`FIRST_CHAIN`].
const CALLER_SPAN: &str = "ae60e65a7062dd18";

/// How long each polled queue hides a message it hands over for, in seconds — above the
/// `Timeout` the functions here carry, which is what a mapping between the two is held to.
const VISIBILITY_TIMEOUT_SECONDS: i64 = 60;

/// The `Timeout` every function this suite creates carries, in seconds. Sized the way the
/// sibling suites that RUN handlers size theirs: it has to cover this test binary's own
/// scheduling, not the handler's work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i64 = 25;

/// How long a row watches for the work the delivery owes — the batch leaving the queue, the
/// invocation running, and its segments reaching X-Ray — before it is a failure.
///
/// PATIENCE, not a weakened expectation: every assertion below is unchanged. The bound is taken
/// from what the EMULATOR itself allows ONE attempt (the wait for a share of the run capacity
/// and the wait for the run's own answer), with room for the several passes a row waits for,
/// because a whole-workspace run shares its cores with every other suite.
const WATCH_BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_add(Duration::from_secs(30));

/// How wide a window the trace search is made over, either side of this suite's own clock: well
/// inside the widest window `GetTraceSummaries` serves and the allowance its `EndTime` has past
/// the clock.
const WINDOW_BEHIND_SECONDS: f64 = 300.0;
/// See [`WINDOW_BEHIND_SECONDS`].
const WINDOW_AHEAD_SECONDS: f64 = 60.0;

/// How many pages of trace summaries a query walks before the paging is called broken.
const PAGE_LIMIT: usize = 32;

/// The handler every function here runs: it returns what it was handed, so a run that happened
/// is a run that answered and nothing of the trace depends on what the handler did.
const HANDLER_SOURCE: &str = r#"
def lambda_handler(event, context):
    return {"records": len((event or {}).get("Records") or [])}
"#;

/// AWS Lambda, Amazon SQS and AWS X-Ray over ONE engine — the three services this chain spans,
/// mounted the way the production router mounts them — with the poll passes of its mappings this
/// suite's own.
fn emulator() -> (MultiServiceRouter, DrivenPolling) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_xray_awsxray::EmulatedService::new(
        Arc::clone(&engine),
    ));
    let driven = DrivenPolling::claiming(&engine, &router);
    (router, driven)
}

/// One AWS X-Ray call over its `restJson1` wire, the way a client makes one.
async fn xray(router: &MultiServiceRouter, path: &str, body: &Value) -> (u16, Value) {
    let host = format!("xray.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("http://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/xray/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the X-Ray request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Create a `TracingConfig` `Active` `python3.12` function running [`HANDLER_SOURCE`] and wait
/// until it reports `Active`, which is when a mapping on it polls.
async fn create_traced_function(router: &MultiServiceRouter, case: &str, name: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            // "Set Mode to Active to sample and trace a subset of incoming requests with
            // X-Ray" (`TracingConfig$Mode`): the mode under which Lambda records a segment for
            // the invocation at all.
            "TracingConfig": { "Mode": "Active" },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(crate::synchronous_invoke_run::package(HANDLER_SOURCE)),
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

/// `SendMessage` of `body` to the queue `url` addresses, carrying `trace_header` as its
/// `AWSTraceHeader` system attribute when there is one.
async fn send_traced_message(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    body: &str,
    trace_header: Option<&str>,
) {
    let mut request = json!({ "QueueUrl": url, "MessageBody": body });
    if let Some(trace_header) = trace_header {
        request["MessageSystemAttributes"] = json!({
            TRACE_ATTRIBUTE: { "DataType": "String", "StringValue": trace_header },
        });
    }
    let (status, sent) = call_sqs(router, "SendMessage", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected SendMessage to answer 200, got {status} {sent}"
    );
}

/// This suite's own clock, as the epoch seconds a trace query states its window in.
fn now_epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default()
}

/// Which traces `GetTraceSummaries` selects for `expression` over a window around this suite's
/// own clock, walking every page of the selection (`POST /TraceSummaries`).
async fn selected(router: &MultiServiceRouter, case: &str, expression: &str) -> Vec<String> {
    let now = now_epoch_seconds();
    let mut found: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..PAGE_LIMIT {
        let mut body = json!({
            "StartTime": now - WINDOW_BEHIND_SECONDS,
            "EndTime": now + WINDOW_AHEAD_SECONDS,
            "FilterExpression": expression,
        });
        if let Some(token) = &token {
            body["NextToken"] = json!(token);
        }
        let (status, page) = xray(router, "/TraceSummaries", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetTraceSummaries for {expression:?} to answer 200, got \
             {status} {page}"
        );
        for summary in page["TraceSummaries"].as_array().cloned().unwrap_or_default() {
            if let Some(id) = summary["Id"].as_str() {
                if !found.iter().any(|held| held == id) {
                    found.push(id.to_string());
                }
            }
        }
        token = page["NextToken"].as_str().map(str::to_string);
        if token.is_none() {
            found.sort();
            return found;
        }
    }
    panic!("case {case:?}: GetTraceSummaries kept issuing a NextToken past {PAGE_LIMIT} pages");
}

/// Every segment document X-Ray holds for `trace_id`, read through `BatchGetTraces` — the
/// operation a client reads a trace back with (`POST /Traces`).
async fn documents_of(router: &MultiServiceRouter, case: &str, trace_id: &str) -> Vec<Value> {
    let (status, answered) = xray(router, "/Traces", &json!({ "TraceIds": [trace_id] })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected BatchGetTraces to answer 200, got {status} {answered}"
    );
    answered["Traces"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .flat_map(|trace| trace["Segments"].as_array().cloned().unwrap_or_default())
        .filter_map(|segment| {
            segment["Document"]
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
        })
        .collect()
}

/// The chains the trace `trace_id` LINKS to, read off every segment of it that is named after
/// `function`: each linked chain with the reference type its link carries.
async fn links_of(
    router: &MultiServiceRouter,
    case: &str,
    trace_id: &str,
    function: &str,
) -> BTreeMap<String, String> {
    let mut links: BTreeMap<String, String> = BTreeMap::new();
    for document in documents_of(router, case, trace_id).await {
        if document["name"].as_str() != Some(function) {
            continue;
        }
        for link in document["links"].as_array().cloned().unwrap_or_default() {
            let named = link["trace_id"].as_str().unwrap_or_default().to_string();
            let reference = link["attributes"][REFERENCE_TYPE_ATTRIBUTE]
                .as_str()
                .unwrap_or_default()
                .to_string();
            links.insert(named, reference);
        }
    }
    links
}

/// Drive this suite's poll passes until the queue `url` addresses holds nothing — the delivery
/// of every message on it has been attempted and succeeded — and answer what it last held.
async fn delivered(
    router: &MultiServiceRouter,
    driven: &mut DrivenPolling,
    case: &str,
    url: &str,
) -> u32 {
    let deadline = Instant::now() + WATCH_BUDGET;
    loop {
        driven.poll_once().await;
        let held = queue_held(router, case, url).await;
        if held == 0 || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The traces `function` has published, waited for until there are `wanted` of them or the
/// deadline passes — the publication is the emulator's own work off the invocation's answer, so
/// the wait is for the CONDITION rather than for a span of the clock.
async fn traces_of(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    wanted: usize,
) -> Vec<String> {
    let expression = format!("service(\"{function}\")");
    let deadline = Instant::now() + WATCH_BUDGET;
    loop {
        let found = selected(router, case, &expression).await;
        if found.len() >= wanted || Instant::now() >= deadline {
            return found;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row: the messages the queue is sent, how many of them one batch may hold, and the chains
/// each published trace must link to.
struct Row {
    case: &'static str,
    /// Each message: its body, and the tracing context its send carries.
    messages: &'static [(&'static str, Option<&'static str>)],
    /// `BatchSize`, "the maximum number of records in each batch that Lambda pulls from your
    /// stream or queue".
    batch_size: i64,
    /// One entry per trace the delivery must publish: the chains that trace links to. The order
    /// is not part of the rule — the rows are compared as sorted sets of link sets.
    links: &'static [&'static [&'static str]],
}

/// The catalog.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one message of one chain",
            messages: &[("first", Some(FIRST_CHAIN))],
            batch_size: 10,
            links: &[&[FIRST_CHAIN]],
        },
        Row {
            case: "two messages of two chains, one message per batch",
            messages: &[("first", Some(FIRST_CHAIN)), ("second", Some(SECOND_CHAIN))],
            batch_size: 1,
            links: &[&[FIRST_CHAIN], &[SECOND_CHAIN]],
        },
        Row {
            case: "two messages of two chains in one batch",
            messages: &[("first", Some(FIRST_CHAIN)), ("second", Some(SECOND_CHAIN))],
            batch_size: 10,
            links: &[&[FIRST_CHAIN, SECOND_CHAIN]],
        },
        Row {
            case: "two messages of ONE chain in one batch",
            messages: &[("first", Some(FIRST_CHAIN)), ("second", Some(FIRST_CHAIN))],
            batch_size: 10,
            links: &[&[FIRST_CHAIN]],
        },
        Row {
            case: "a message carrying no chain X-Ray reads",
            messages: &[("first", None), ("second", Some("Root=not-a-trace-id;Sampled=1"))],
            batch_size: 10,
            links: &[&[]],
        },
    ]
}

/// The tracing context a producer stamps for `chain`, as X-Ray writes one.
fn trace_header(chain: &str) -> String {
    format!("Root={chain};Parent={CALLER_SPAN};Sampled=1")
}

/// RULE: a delivery an event source mapping makes to a `TracingConfig` `Active` function runs
/// under a chain of its own and links back to each distinct chain the delivered messages
/// belonged to, with `parent` as the reference type; a batch whose messages belong to no chain
/// links back to none.
#[tokio::test]
async fn a_delivered_batch_links_its_invocations_trace_back_to_the_messages_chains() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let (router, mut driven) = emulator();
        let function = format!("sqs-trace-links-{index}");
        let (url, arn) = create_queue(
            &router,
            case,
            &format!("sqs-trace-links-{index}"),
            &json!({ "VisibilityTimeout": VISIBILITY_TIMEOUT_SECONDS.to_string() }),
        )
        .await;
        create_traced_function(&router, case, &function).await;
        given_enabled_mapping(
            &router,
            case,
            &json!({
                "FunctionName": function,
                "EventSourceArn": arn,
                "BatchSize": row.batch_size,
                "Enabled": true,
            }),
        )
        .await;

        // The chains the messages name, each written the way a producer stamps one. A row's
        // message either carries a context or carries none at all.
        let mut named: Vec<String> = Vec::new();
        for (body, chain) in row.messages {
            let supplied = match chain {
                Some(chain) if chain.starts_with("Root=") => (*chain).to_string(),
                Some(chain) => trace_header(chain),
                None => String::new(),
            };
            let header = (!supplied.is_empty()).then(|| supplied.clone());
            send_traced_message(&router, case, &url, body, header.as_deref()).await;
            if let Some(header) = header {
                named.push(header);
            }
        }

        let held = delivered(&router, &mut driven, case, &url).await;
        assert_eq!(
            held, 0,
            "case {case:?}: every message must be delivered and leave the queue, {held} are \
             still held"
        );

        // What a client reads: the traces the function's own name selects, then each of them.
        let wanted = row.links.len();
        let traces = traces_of(&router, case, &function, wanted).await;
        assert_eq!(
            traces.len(),
            wanted,
            "case {case:?}: expected {wanted} trace(s) for service({function:?}), got {traces:?}"
        );

        let mut reported: Vec<Vec<(String, String)>> = Vec::new();
        for trace in &traces {
            // The invocation runs under a chain of its OWN: a message's chain is referred to,
            // never continued.
            for header in &named {
                let chain = header
                    .split(';')
                    .find_map(|field| field.strip_prefix("Root="))
                    .unwrap_or_default();
                assert_ne!(
                    trace.as_str(),
                    chain,
                    "case {case:?}: the delivery must publish a trace of its own, it published \
                     the message's chain {chain:?}"
                );
            }
            let links = links_of(&router, case, trace, &function).await;
            reported.push(links.into_iter().collect());
        }
        let mut expected: Vec<Vec<(String, String)>> = row
            .links
            .iter()
            .map(|chains| {
                let mut entries: Vec<(String, String)> = chains
                    .iter()
                    .map(|chain| ((*chain).to_string(), PARENT_REFERENCE.to_string()))
                    .collect();
                entries.sort();
                entries
            })
            .collect();
        expected.sort();
        reported.sort();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the published traces to link to {expected:?}, got \
             {reported:?} over {traces:?}"
        );
    }
}
