// SPDX-License-Identifier: CC0-1.0

//! MODEL INVOCATION LOGGING into a CloudWatch Logs destination, end to end over
//! HTTP: the log event a served inference puts in the log group this account's
//! logging configuration names.
//!
//! The RULE asserted here, over a SET of inputs rather than over the one call that
//! showed it: while a logging configuration naming a log group destination is in
//! force, EVERY inference this data plane serves puts ONE log event in that group,
//! whose message is the invocation's own record — the schema it is, the operation
//! that served it, the model it resolved to, the account and Region it was served
//! for, the bodies the call carried and the token counts the API reported inline —
//! and while none is installed, or once it has been withdrawn, nothing is put in
//! the group at all. A configuration names its destinations independently, so each
//! destination it names receives the record and neither supplies the other.
//!
//! The rows differ along the surface that served the inference (the two Converse
//! surfaces and the two invoke surfaces, unary and streaming), along the form of
//! the log group name the destination was configured with, along the per-datatype
//! delivery switch that decides whether the prompt and the completion travel, and
//! along which destinations the configuration names (the group alone, the bucket
//! alone, both).
//!
//! CloudWatch Logs and Amazon S3 are mounted beside this service on one engine —
//! the way the assembled emulator runs them — because a delivered event is an event
//! of that store: it is read back here through CloudWatch Logs' own
//! `FilterLogEvents`, exactly as a client polling for its invocation's record reads
//! it.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice as bedrock;
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state::invocation_logging as logging;
use smithy_server_runtime::MultiServiceRouter;

use crate::bedrock_runtime_inference::{post_json, send, Call};
use crate::model_invocation_log_delivery::{delivered_keys, given_bucket, BUCKET};

/// The Region every request below is signed for, which is the Region a delivered
/// record names and the endpoint its log event is written on.
const REGION: &str = "us-east-1";

/// A model both Converse surfaces and both invoke surfaces serve.
const MODEL: &str = "amazon.nova-lite-v1:0";

/// The log group every destination below delivers into, unless a row names its own.
const LOG_GROUP: &str = "/aws/bedrock/model-invocations";

/// The number of `FilterLogEvents` pages one case's reader walks before it gives
/// up: a case serves a handful of invocations, and a page that carried events is
/// followed by at most one that carries none, so this is far above what any row
/// needs and bounds a token the store might keep issuing.
const MAX_PAGES: usize = 8;

/// The account the emulator serves, which every delivered record names.
fn account_id() -> String {
    ::engine::emulator_account_id().to_string()
}

/// The role a CloudWatch Logs destination is configured with. The control plane
/// settles whether such a role may be passed before it installs the destination;
/// this suite drives the install seam, so the value is only carried.
fn delivery_role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/BedrockModelInvocationLogging",
        account_id()
    )
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

/// This service, CloudWatch Logs and Amazon S3 on one shared engine, behind the
/// production router. The engine comes back so a test can install the logging
/// destination the control plane's own put installs — this crate cannot call that
/// control plane (it is the crate that depends on THIS one), so the install seam is
/// driven directly.
fn emulator() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(bedrock::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (engine, router)
}

/// One CloudWatch Logs call (`awsJson1_1`: `POST /` with the operation in
/// `X-Amz-Target`), asserting a 200 and answering the body.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://logs.{REGION}.amazonaws.com/"))
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR000000000/20260101/{REGION}/logs/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    assert_eq!(
        status,
        200,
        "case {case:?}: logs {operation} must be served, got {status} / {}",
        String::from_utf8_lossy(&payload)
    );
    if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: logs {operation} answered a body that is not JSON ({error}): {}",
                String::from_utf8_lossy(&payload)
            )
        })
    }
}

/// A log group called `name`, as the account's owner creates it before pointing a
/// logging configuration at it — asserted to be held by CloudWatch Logs, which is
/// what the control plane resolves before it admits such a destination.
async fn given_log_group(router: &MultiServiceRouter, case: &str, name: &str) {
    logs(router, case, "CreateLogGroup", &json!({ "logGroupName": name })).await;
    let listed = logs(
        router,
        case,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": name }),
    )
    .await;
    let held = listed["logGroups"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .any(|group| group["logGroupName"].as_str() == Some(name))
        })
        .unwrap_or(false);
    assert!(
        held,
        "case {case:?}: CloudWatch Logs must hold the group {name:?}, got {listed}"
    );
}

/// Every log event the group `name` holds, as `FilterLogEvents` reports them,
/// following the token it issues so a case reads the whole group.
async fn log_events(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<Value> {
    let mut events: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    for page in 0..MAX_PAGES {
        let mut request = json!({ "logGroupName": name });
        if let Some(token) = next.as_ref() {
            request["nextToken"] = json!(token);
        }
        let answer = logs(router, case, "FilterLogEvents", &request).await;
        let page_events = answer["events"].as_array().cloned().unwrap_or_default();
        let carried = page_events.len();
        events.extend(page_events);
        next = answer["nextToken"].as_str().map(str::to_string);
        if carried == 0 || next.is_none() {
            return events;
        }
        assert!(
            page + 1 < MAX_PAGES,
            "case {case:?}: expected the group {name:?} to be read within {MAX_PAGES} pages, \
             got {} event(s) and a further token",
            events.len()
        );
    }
    events
}

/// The records the group `name` holds: one per log event, its `message` parsed as
/// the JSON document a model invocation log event carries.
async fn delivered_records(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<Value> {
    log_events(router, case, name)
        .await
        .iter()
        .map(|event| {
            let message = event["message"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: a delivered log event carries no message: {event}")
            });
            assert_eq!(
                event["logStreamName"].as_str(),
                Some(logging::LOG_STREAM_NAME),
                "case {case:?}: expected the event in the invocation-log stream, got {event}"
            );
            serde_json::from_str(message).unwrap_or_else(|error| {
                panic!("case {case:?}: a delivered record is not JSON ({error}): {message:?}")
            })
        })
        .collect()
}

/// The ONE record the group `name` holds.
async fn sole_record(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let records = delivered_records(router, case, name).await;
    assert_eq!(
        records.len(),
        1,
        "case {case:?}: expected exactly one delivered record in {name:?}, got {records:?}"
    );
    records[0].clone()
}

/// Put a logging configuration in force, the way the control plane's admitted
/// `PutModelInvocationLoggingConfiguration` puts it: `log_group` is the
/// `cloudWatchConfig` destination and `bucket` the `s3Config` one, each named only
/// where the row names it.
fn given_logging(
    engine: &Arc<engine::InMemoryEngine>,
    log_group: Option<&str>,
    bucket: Option<&str>,
    text_data_delivery_enabled: bool,
) {
    assert!(
        logging::install(
            &**engine,
            Some(logging::InvocationLogDelivery {
                object_destination: bucket.map(|bucket| logging::ObjectDestination {
                    bucket: bucket.to_string(),
                    key_prefix: None,
                }),
                log_group_destination: log_group.map(|log_group| logging::LogGroupDestination {
                    log_group: log_group.to_string(),
                    role_arn: delivery_role_arn(),
                }),
                text_data_delivery_enabled,
                image_data_delivery_enabled: false,
                embedding_data_delivery_enabled: false,
            })
        ),
        "the logging destination must install on this engine"
    );
}

// ---------------------------------------------------------------------------
// The surfaces
// ---------------------------------------------------------------------------

/// One inference surface: how a call reaches it, what the record it delivers has to
/// say it was, and the log group this row points the destination at.
struct Surface {
    case: &'static str,
    operation: &'static str,
    /// The path the call is posted to, under `/model/<modelId>/…`.
    path: &'static str,
    /// Whether the request body is the Converse-API shape (as opposed to the
    /// model's own native invoke body).
    converse_shaped: bool,
}

/// The four surfaces that serve an inference, which is the set the rule covers:
/// the unary and streaming Converse APIs, and the unary and streaming invoke APIs.
fn surfaces() -> Vec<Surface> {
    vec![
        Surface {
            case: "Converse",
            operation: logging::OPERATION_CONVERSE,
            path: "converse",
            converse_shaped: true,
        },
        Surface {
            case: "ConverseStream",
            operation: logging::OPERATION_CONVERSE_STREAM,
            path: "converse-stream",
            converse_shaped: true,
        },
        Surface {
            case: "InvokeModel",
            operation: logging::OPERATION_INVOKE_MODEL,
            path: "invoke",
            converse_shaped: false,
        },
        Surface {
            case: "InvokeModelWithResponseStream",
            operation: logging::OPERATION_INVOKE_MODEL_WITH_RESPONSE_STREAM,
            path: "invoke-with-response-stream",
            converse_shaped: false,
        },
    ]
}

/// Serve one inference on `surface`, carrying `prompt`.
async fn invoke(router: &MultiServiceRouter, surface: &Surface, prompt: &str) -> Call {
    let body = if surface.converse_shaped {
        json!({ "messages": [{ "role": "user", "content": [{ "text": prompt }] }] })
    } else {
        json!({
            "messages": [{ "role": "user", "content": [{ "text": prompt }] }],
            "inferenceConfig": { "maxTokens": 64 },
        })
    };
    let call = if surface.converse_shaped {
        post_json(router, &format!("/model/{MODEL}/{}", surface.path), &body).await
    } else {
        send(
            router,
            "POST",
            &format!("/model/{MODEL}/{}", surface.path),
            &[("content-type", "application/json")],
            serde_json::to_vec(&body).expect("a serializable test body"),
        )
        .await
    };
    assert_eq!(
        call.status,
        200,
        "case {:?}: the inference must be served, got {} / {}",
        surface.case,
        call.status,
        String::from_utf8_lossy(&call.body)
    );
    call
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// RULE: every inference surface puts one log event in the log group the
/// configuration names, whose message is the invocation's record — the schema it
/// is, the operation that served it, the model it resolved to, and the account and
/// Region it was served for — carrying the request the call arrived with and the
/// answer it served.
///
/// The group names span the forms CloudWatch Logs holds a group under (a path-like
/// name, a flat one, one carrying the punctuation its published domain admits), so
/// the group WRITTEN is the group the configuration named rather than one this
/// emulator picks.
#[tokio::test]
async fn every_inference_surface_puts_a_log_event_in_the_configured_log_group() {
    // The group each row points its destination at, cycled over the surfaces.
    let group_names = [
        "/aws/vendedlogs/bedrock/model-invocations",
        "bedrock-invocations",
        "Audit.Bedrock_Logs/2026#1",
        "b",
    ];
    for (index, surface) in surfaces().iter().enumerate() {
        let case = surface.case;
        let group = group_names[index % group_names.len()];
        let (engine, router) = emulator();
        given_log_group(&router, case, group).await;
        given_logging(&engine, Some(group), None, true);

        // A prompt whose text is this case's own, so a record carrying it cannot be
        // one delivered for another call.
        let prompt = format!("say something about {case} and nothing else");
        invoke(&router, surface, &prompt).await;

        let record = sole_record(&router, case, group).await;
        assert_eq!(
            (
                record["schemaType"].as_str(),
                record["operation"].as_str(),
                record["modelId"].as_str(),
                record["region"].as_str(),
                record["accountId"].as_str(),
            ),
            (
                Some(logging::SCHEMA_TYPE),
                Some(surface.operation),
                Some(MODEL),
                Some(REGION),
                Some(account_id().as_str()),
            ),
            "case {case:?}: the record must state the call it is of, got {record}"
        );
        assert!(
            !record["requestId"].as_str().unwrap_or_default().is_empty(),
            "case {case:?}: the record must name the invocation it is of, got {record}"
        );
        let input_body = record["input"]["inputBodyJson"].to_string();
        assert!(
            input_body.contains(&prompt),
            "case {case:?}: expected the served prompt in the logged request body, got \
             {input_body}"
        );
        assert!(
            !record["output"]["outputBodyJson"].is_null(),
            "case {case:?}: expected the served answer in the record, got {record}"
        );
    }
}

/// RULE: the token counts a delivered log event carries are the ones the API
/// reported INLINE for that same call, so a client reconciling its audit trail
/// against the response it read finds the same accounting.
#[tokio::test]
async fn a_delivered_log_event_carries_the_token_counts_the_call_reported_inline() {
    // The two unary surfaces report their accounting in the response body a caller
    // reads, which is what the record is held against here. The rows differ along
    // the envelope that reports it and along the length of the prompt, so the counts
    // differ between them.
    // (case, path, converse-shaped, prompt)
    let rows: [(&str, &str, bool, &str); 3] = [
        ("Converse, a short prompt", "converse", true, "hello"),
        (
            "Converse, a longer prompt",
            "converse",
            true,
            "name one primary colour and then say nothing else at all about it",
        ),
        (
            "InvokeModel, the model's own envelope",
            "invoke",
            false,
            "name one primary colour",
        ),
    ];
    for (case, path, converse_shaped, prompt) in rows {
        let (engine, router) = emulator();
        given_log_group(&router, case, LOG_GROUP).await;
        given_logging(&engine, Some(LOG_GROUP), None, true);

        let surface = Surface {
            case,
            operation: logging::OPERATION_CONVERSE,
            path,
            converse_shaped,
        };
        let answered = invoke(&router, &surface, prompt).await.json();
        // Both envelopes report the accounting under the same two members, so one
        // reading covers the rows.
        let inline = (
            answered["usage"]["inputTokens"].as_i64(),
            answered["usage"]["outputTokens"].as_i64(),
        );
        assert!(
            inline.0.is_some() && inline.1.is_some(),
            "case {case:?}: the call must report its accounting inline, got {answered}"
        );

        let record = sole_record(&router, case, LOG_GROUP).await;
        assert_eq!(
            (
                record["input"]["inputTokenCount"].as_i64(),
                record["output"]["outputTokenCount"].as_i64(),
            ),
            inline,
            "case {case:?}: the record must carry the counts the call reported inline \
             ({inline:?}), got {record}"
        );
    }
}

/// RULE: the per-datatype delivery switch decides only how much of a delivered
/// event travels — the event itself still arrives, with the operation, the model
/// and the counts — never whether one arrives.
#[tokio::test]
async fn the_text_delivery_switch_decides_only_how_much_of_a_log_event_travels() {
    // (case, text data delivery, the bodies travel)
    let rows: [(&str, bool, bool); 2] = [
        ("text data delivery enabled", true, true),
        ("text data delivery disabled", false, false),
    ];
    for (case, text_enabled, bodies) in rows {
        let (engine, router) = emulator();
        given_log_group(&router, case, LOG_GROUP).await;
        given_logging(&engine, Some(LOG_GROUP), None, text_enabled);

        let prompt = format!("a prompt for {case}");
        invoke(&router, &surfaces()[0], &prompt).await;

        let record = sole_record(&router, case, LOG_GROUP).await;
        assert_eq!(
            record["operation"].as_str(),
            Some(logging::OPERATION_CONVERSE),
            "case {case:?}: the event must arrive whatever the switches say, got {record}"
        );
        assert!(
            record["input"]["inputTokenCount"].as_i64().is_some()
                && record["output"]["outputTokenCount"].as_i64().is_some(),
            "case {case:?}: the event must carry its counts whatever the switches say, got \
             {record}"
        );
        assert_eq!(
            (
                !record["input"]["inputBodyJson"].is_null(),
                !record["output"]["outputBodyJson"].is_null(),
            ),
            (bodies, bodies),
            "case {case:?}: expected the bodies present={bodies}, got {record}"
        );
        if !bodies {
            let rendered = record.to_string();
            assert!(
                !rendered.contains(&prompt),
                "case {case:?}: an event with text delivery off must carry no prompt, got \
                 {rendered}"
            );
        }
    }
}

/// RULE: a configuration names its destinations independently, so each one it names
/// receives the invocation's record and neither supplies nor suppresses the other:
/// a configuration naming only the log group is delivered to as much as one naming
/// only the bucket, and one naming both gets the same record in each form.
#[tokio::test]
async fn each_destination_a_configuration_names_receives_the_invocations_record() {
    // (case, the log group the configuration names, the bucket it names)
    let rows: [(&str, bool, bool); 3] = [
        ("the log group alone", true, false),
        ("the bucket alone", false, true),
        ("both destinations at once", true, true),
    ];
    for (case, names_group, names_bucket) in rows {
        let (engine, router) = emulator();
        given_log_group(&router, case, LOG_GROUP).await;
        given_bucket(&router, case).await;
        given_logging(
            &engine,
            names_group.then_some(LOG_GROUP),
            names_bucket.then_some(BUCKET),
            true,
        );

        let prompt = format!("a prompt for {case}");
        invoke(&router, &surfaces()[0], &prompt).await;

        let records = delivered_records(&router, case, LOG_GROUP).await;
        assert_eq!(
            records.len(),
            usize::from(names_group),
            "case {case:?}: expected {} log event(s) in the group, got {records:?}",
            usize::from(names_group)
        );
        let keys = delivered_keys(&router, case, "").await;
        assert_eq!(
            keys.len(),
            usize::from(names_bucket),
            "case {case:?}: expected {} delivered object(s), got {keys:?}",
            usize::from(names_bucket)
        );
        for record in &records {
            assert_eq!(
                record["operation"].as_str(),
                Some(logging::OPERATION_CONVERSE),
                "case {case:?}: the delivered record must state the call it is of, got {record}"
            );
        }
    }
}

/// RULE: delivery follows the configuration. An account that configured no
/// destination has nothing put in a group it holds, and an account whose
/// configuration was withdrawn has nothing put there for the calls it serves after
/// the withdrawal — which is what makes "logging is configured" and "logging works"
/// two observable states rather than one.
#[tokio::test]
async fn nothing_is_put_in_the_group_without_a_configuration_and_nothing_further_after_one_is_withdrawn(
) {
    // (case, install a destination before the first call, withdraw it after it)
    let rows: [(&str, bool, bool); 3] = [
        ("no destination was ever configured", false, false),
        ("a destination stands", true, false),
        ("the destination was withdrawn", true, true),
    ];
    for (case, installed, withdrawn) in rows {
        let (engine, router) = emulator();
        given_log_group(&router, case, LOG_GROUP).await;
        if installed {
            given_logging(&engine, Some(LOG_GROUP), None, true);
        }

        invoke(&router, &surfaces()[0], "the first call").await;
        let after_first = delivered_records(&router, case, LOG_GROUP).await.len();
        assert_eq!(
            after_first,
            usize::from(installed),
            "case {case:?}: expected {} event(s) for the first call, got {after_first}",
            usize::from(installed)
        );

        if withdrawn {
            assert!(
                logging::install(&*engine, None),
                "case {case:?}: the destination must be withdrawable"
            );
        }
        invoke(&router, &surfaces()[0], "the second call").await;
        let after_second = delivered_records(&router, case, LOG_GROUP).await.len();
        let expected = match (installed, withdrawn) {
            // Withdrawn: the first call's event stands, and the second puts none.
            (true, true) => 1,
            (true, false) => 2,
            _ => 0,
        };
        assert_eq!(
            after_second, expected,
            "case {case:?}: expected {expected} event(s) after the second call, got \
             {after_second}"
        );
    }
}

/// RULE: the group a destination names is the ACCOUNT's, so a configuration naming
/// one CloudWatch Logs does not hold loses the records rather than bringing a group
/// into existence underneath its owner — and the inference it was serving is
/// answered either way, because delivery happens after the call has been served.
#[tokio::test]
async fn a_destination_naming_a_group_cloudwatch_logs_does_not_hold_creates_none() {
    let case = "the configured log group was never created";
    let absent = "/aws/bedrock/never-created";
    let (engine, router) = emulator();
    given_logging(&engine, Some(absent), None, true);

    invoke(&router, &surfaces()[0], "a call whose destination is absent").await;

    let listed = logs(
        &router,
        case,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": absent }),
    )
    .await;
    let groups = listed["logGroups"].as_array().cloned().unwrap_or_default();
    assert!(
        groups.is_empty(),
        "case {case:?}: expected the account to hold no such group, got {listed}"
    );
}
