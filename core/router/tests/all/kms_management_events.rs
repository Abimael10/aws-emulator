// SPDX-License-Identifier: CC0-1.0

//! AWS KMS calls in CloudTrail event history, joined to the response each caller was
//! answered with, across two services on one engine.
//!
//! RULE: every response an AWS service sends names the request it answered — the id an SDK
//! surfaces as `ResponseMetadata.RequestId` — and the service records that call as a
//! management event of its own carrying the SAME id as the record's `requestID`, so a client
//! holding a response can find the record of that very call, and one record exists per call.
//! For KMS that id closes a three-way join the API is defined on: `KeyLastUsageData` reports
//! "the request ID of the cryptographic operation" as `KmsRequestId` and "the CloudTrail
//! event ID of the cryptographic operation" as `CloudTrailEventId`
//! (`smithy_query(query="shape",shape="KeyLastUsageData")`), so the key's usage record names
//! the caller's own request and the event a lookup reports for it.
//!
//! What the record carries of a cryptographic call is what says WHICH key and under what
//! terms — the key id, the encryption context, the algorithm — and never the material: the
//! plaintext members the model marks `@sensitive` are left OUT of the document entirely
//! (`kms::OMITTED_MEMBERS`), so the encryption context of an `Encrypt` is observable at the
//! trail while its plaintext is nowhere in it.
//!
//! Only a cross-service suite can pin this: the records are written by the service that
//! SERVES the call (KMS, awsJson1_1) and read back through `cloudtrail:LookupEvents`
//! (awsJson1_1) — two services over ONE engine, as `core/router/src/lib.rs` mounts them. The
//! response-side half of the join (every class of KMS answer naming its request, and the
//! usage record reporting that id) is pinned in the serving crate's own suite
//! (`services/service-com-amazonaws-kms-trentservice/tests/all/response_request_id.rs`).
//!
//! The rows span what the rule generalises over rather than the one call that motivated it:
//! a reporting cryptographic call, several calls to ONE operation (which is what makes the
//! per-call count observable), a mutating control-plane call, a call that was REFUSED, and a
//! use of a key that no client made at all — the in-process call another service makes for
//! the caller it is serving.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_kms_trentservice::service::management_events::{
    EVENT_SOURCE as KMS_EVENT_SOURCE, OMITTED_MEMBERS,
};
use service_com_amazonaws_kms_trentservice::state::USAGE_TRACKING_LAG;

/// The Region every call below is signed for, and so the Region the records live in.
const REGION: &str = "us-west-2";

/// How wide a window the lookups below ask for, in seconds either side of the call.
/// Generous on purpose: the window is here to exercise `StartTime` / `EndTime`, not to time
/// anything, so nothing about the suite depends on how fast the host runs.
const LOOKUP_WINDOW_SECONDS: i64 = 3_600;

/// A key id of the right form that this account holds no key for, so the refused row below
/// is the modelled "not found" rather than a malformed identifier.
const ABSENT_KEY_ID: &str = "11111111-2222-3333-4444-555555555555";

/// The plaintext every `Encrypt` below seals. A value that appears nowhere else, so finding
/// it in a record would be unambiguous.
const PLAINTEXT: &[u8] = b"not-in-the-trail-plaintext";

/// The services this join travels through, over ONE shared engine: KMS serves and records
/// the calls, CloudTrail reads the records back, and Amazon SQS is the service that reaches
/// KMS in-process for the caller of one of its own calls.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential scope
    // is still carried, because it is what routes the request and names the Region it was
    // served in.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` and [`REGION`], which is
/// what routes the request and what names the Region it was served in. The signature is
/// never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDKMSAUDIT/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// What one call answered: its status, its body, and the request its response named.
struct Answered {
    status: u16,
    body: Value,
    /// The `x-amzn-RequestId` the response carried — matched case-insensitively, as HTTP
    /// matches a field name.
    request_id: String,
}

/// One RPC call to `signing_name`, dispatched with `target_prefix` naming its API and
/// `content_type` naming its awsJson dialect.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    content_type: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> Answered {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let request_id = response
        .headers()
        .get("x-amzn-requestid")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let status = response.status().as_u16();
    let body = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    Answered {
        status,
        body,
        request_id,
    }
}

/// One `awsJson1_1` call to `signing_name` — the dialect both services this suite reads
/// speak.
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> Answered {
    rpc(
        router,
        signing_name,
        "application/x-amz-json-1.1",
        target_prefix,
        operation,
        body,
    )
    .await
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must be served.
async fn sqs_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Answered {
    let answered = rpc(
        router,
        "sqs",
        "application/x-amz-json-1.0",
        "AmazonSQS",
        operation,
        body,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: Amazon SQS {operation} must be served, got {} with {}",
        answered.status, answered.body
    );
    answered
}

/// One KMS call.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answered {
    call(router, "kms", "TrentService", operation, body).await
}

/// One KMS call that must be served.
async fn kms_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Answered {
    let answered = kms(router, operation, body).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: KMS {operation} must be served, got {} with {}",
        answered.status, answered.body
    );
    assert!(
        !answered.request_id.is_empty(),
        "case {case:?}: KMS {operation} must name the request it answered, got no \
         x-amzn-RequestId"
    );
    answered
}

/// `cloudtrail:LookupEvents` filtered on one attribute over a window holding every call this
/// suite makes, as each matching event: the reported `Event` and its parsed
/// `CloudTrailEvent` document.
async fn looked_up(
    router: &MultiServiceRouter,
    attribute_key: &str,
    attribute_value: &str,
) -> Vec<(Value, Value)> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock at or after the epoch")
        .as_secs() as i64;
    let answered = call(
        router,
        "cloudtrail",
        "CloudTrail_20131101",
        "LookupEvents",
        &json!({
            "LookupAttributes": [{
                "AttributeKey": attribute_key,
                "AttributeValue": attribute_value,
            }],
            "StartTime": now - LOOKUP_WINDOW_SECONDS,
            "EndTime": now + LOOKUP_WINDOW_SECONDS,
            "MaxResults": 50,
        }),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "LookupEvents({attribute_key}={attribute_value}) must be served, got {} with {}",
        answered.status, answered.body
    );
    answered.body["Events"]
        .as_array()
        .unwrap_or_else(|| panic!("LookupEvents reports an Events list, got {}", answered.body))
        .iter()
        .map(|event| {
            let document = event["CloudTrailEvent"]
                .as_str()
                .unwrap_or_else(|| panic!("an event carries its CloudTrailEvent: {event}"));
            let parsed: Value = serde_json::from_str(document)
                .unwrap_or_else(|error| panic!("CloudTrailEvent is JSON ({error}): {document}"));
            (event.clone(), parsed)
        })
        .collect()
}

/// The ONE event among `events` whose record names the request answered under `request_id` —
/// how a client holding its own response finds the record of that very call.
fn by_request_id<'a>(case: &str, events: &'a [(Value, Value)], request_id: &str) -> &'a (Value, Value) {
    let found: Vec<&(Value, Value)> = events
        .iter()
        .filter(|(_, record)| record.get("requestID") == Some(&json!(request_id)))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one record naming the request {request_id:?}, got \
         {events:?}"
    );
    found[0]
}

/// Whether `document` holds `member` as a key anywhere in it, at any depth.
fn names_member_anywhere(document: &Value, member: &str) -> bool {
    match document {
        Value::Object(members) => members
            .iter()
            .any(|(name, value)| name == member || names_member_anywhere(value, member)),
        Value::Array(values) => values
            .iter()
            .any(|value| names_member_anywhere(value, member)),
        _ => false,
    }
}

/// The member name a record spells a modelled member with: the modelled name with a
/// lower-case first letter, which is how a CloudTrail event document names the members of
/// the call it records
/// (`service_common::management_events::recorded_member_name`, asked of the rule rather than
/// restated: the same transformation applied here to the serving crate's own list).
fn recorded_member_name(modelled: &str) -> String {
    let mut chars = modelled.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
    }
}

/// The usage record `GetKeyLastUsage` reports for `key_id` once KMS's usage tracking has
/// PRODUCED one naming `operation` — under `request_id` when the caller knows which call's
/// record it is waiting for.
///
/// The tracking takes a span to begin recording a key (`state::USAGE_TRACKING_LAG`), so the
/// condition is polled until it holds rather than waited out: the deadline is many times
/// that span, so nothing here depends on how fast the host runs.
async fn tracked_usage(
    router: &MultiServiceRouter,
    case: &str,
    key_id: &str,
    operation: &str,
    request_id: Option<&str>,
) -> Value {
    let deadline = Instant::now() + USAGE_TRACKING_LAG * 20;
    loop {
        let answered = kms_ok(router, case, "GetKeyLastUsage", &json!({"KeyId": key_id})).await;
        let record = answered.body["KeyLastUsage"].clone();
        let named = request_id.is_none_or(|id| record["KmsRequestId"] == json!(id));
        if record["Operation"] == json!(operation) && named {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the usage tracking must produce the record of the {operation} \
             answered under {request_id:?}, got {record}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The rule: each KMS call is one management event of `kms.amazonaws.com` naming the request
/// its caller was answered under, the encryption context it carried is observable there
/// while its plaintext is not, and the key's usage record names that very event.
#[tokio::test]
async fn kms_calls_are_in_event_history_under_the_request_their_caller_was_answered_under() {
    let router = router();

    // A MUTATING control-plane call: its record names its own request and carries what the
    // call answered with.
    let created = kms_ok(
        &router,
        "the create",
        "CreateKey",
        &json!({"Description": "audited cryptography"}),
    )
    .await;
    let key_id = created.body["KeyMetadata"]["KeyId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateKey answers a KeyId, got {}", created.body))
        .to_string();

    let creates = looked_up(&router, "EventName", "CreateKey").await;
    let (_, create_record) = by_request_id("the create", &creates, &created.request_id);
    assert_eq!(
        create_record.get("eventSource"),
        Some(&json!(KMS_EVENT_SOURCE)),
        "the record names the serving service, got {create_record}"
    );
    assert_eq!(
        create_record.get("readOnly"),
        Some(&json!(false)),
        "a create changes what the account holds, got {create_record}"
    );
    assert!(
        create_record["responseElements"]["keyMetadata"]["keyId"] == json!(key_id),
        "a served create records what it answered with, got {create_record}"
    );

    // Three REPORTING cryptographic calls to one operation, each with an encryption context
    // of its own: what makes the per-call count and the context observable at the trail.
    let contexts: Vec<Value> = vec![
        json!({"purpose": "first", "tier": "test", "n": "1"}),
        json!({"purpose": "second", "tier": "test", "n": "2"}),
        json!({}),
    ];
    let mut encrypts: Vec<(Value, String)> = Vec::new();
    for context in &contexts {
        let mut request = json!({
            "KeyId": key_id,
            "Plaintext": aws_smithy_types::base64::encode(PLAINTEXT),
        });
        if !context.as_object().is_some_and(serde_json::Map::is_empty) {
            request["EncryptionContext"] = context.clone();
        }
        let answered = kms_ok(&router, "an encrypt", "Encrypt", &request).await;
        encrypts.push((context.clone(), answered.request_id));
    }

    let recorded = looked_up(&router, "EventName", "Encrypt").await;
    assert_eq!(
        recorded.len(),
        encrypts.len(),
        "one record per call: {} Encrypt calls must be {} records, got {recorded:?}",
        encrypts.len(),
        encrypts.len()
    );

    for (context, request_id) in &encrypts {
        let case = format!("the encrypt under {context}");
        let (event, record) = by_request_id(&case, &recorded, request_id);
        assert_eq!(
            record.get("eventSource"),
            Some(&json!(KMS_EVENT_SOURCE)),
            "case {case:?}: the record names the serving service, got {record}"
        );
        assert_eq!(
            record.get("eventName"),
            Some(&json!("Encrypt")),
            "case {case:?}: the record names the operation, got {record}"
        );
        assert_eq!(
            record.get("readOnly"),
            Some(&json!(true)),
            "case {case:?}: using a key changes nothing the account holds, got {record}"
        );
        assert_eq!(
            record.get("errorCode"),
            None,
            "case {case:?}: a served call carries no error code at all, got {record}"
        );
        assert_eq!(
            record["requestParameters"]["keyId"],
            json!(key_id),
            "case {case:?}: the record says which key the call addressed, got {record}"
        );
        let reported_context = record["requestParameters"]
            .get("encryptionContext")
            .cloned()
            .unwrap_or(Value::Null);
        let expected_context = if context.as_object().is_some_and(serde_json::Map::is_empty) {
            // A call that carried no context has none to report: an absent member is left
            // out of the record rather than written as an empty one.
            Value::Null
        } else {
            context.clone()
        };
        assert_eq!(
            reported_context, expected_context,
            "case {case:?}: the encryption context is observable at the trail, got {record}"
        );
        // The material itself is nowhere in the record — neither as a value nor as a key
        // carrying a placeholder.
        for modelled in OMITTED_MEMBERS {
            let member = recorded_member_name(modelled);
            assert!(
                !names_member_anywhere(record, &member),
                "case {case:?}: a record of this service names no {member} anywhere, got \
                 {record}"
            );
        }
        assert!(
            !record.to_string().contains("not-in-the-trail-plaintext"),
            "case {case:?}: the plaintext reaches no record, got {record}"
        );
        assert!(
            event["EventId"].as_str().is_some_and(|id| !id.is_empty()),
            "case {case:?}: a reported event is named by its own EventId, got {event}"
        );
    }

    // The three-way join: the key's usage record names the last call's request and the very
    // event a lookup reports for it.
    let (_, last_request_id) = encrypts
        .last()
        .expect("the rows above made at least one call");
    let usage = tracked_usage(
        &router,
        "the last encrypt",
        &key_id,
        "Encrypt",
        Some(last_request_id),
    )
    .await;
    let (last_event, _) = by_request_id("the last encrypt", &recorded, last_request_id);
    assert_eq!(
        usage["CloudTrailEventId"],
        last_event["EventId"],
        "the usage record names the CloudTrail event of that call, got {usage} against \
         {last_event}"
    );
    // And that id finds the same record on its own, which is the lookup a client holding the
    // usage record makes.
    let by_event_id = looked_up(
        &router,
        "EventId",
        usage["CloudTrailEventId"].as_str().unwrap_or_default(),
    )
    .await;
    assert_eq!(
        by_event_id.len(),
        1,
        "the CloudTrailEventId of a usage record names one event, got {by_event_id:?}"
    );
    assert_eq!(
        by_event_id[0].1.get("requestID"),
        Some(&json!(last_request_id)),
        "that event is the record of the call the usage record describes, got {:?}",
        by_event_id[0].1
    );

    // A REFUSED call is recorded too, with the fault the caller saw — and it used no key, so
    // it took the usage record over from nothing.
    let refused = kms(
        &router,
        "Encrypt",
        &json!({
            "KeyId": ABSENT_KEY_ID,
            "Plaintext": aws_smithy_types::base64::encode(PLAINTEXT),
        }),
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "a call naming a key that is not there is refused, got {} with {}",
        refused.status, refused.body
    );
    assert!(
        !refused.request_id.is_empty(),
        "a refusal names the request it answered too, got no x-amzn-RequestId"
    );
    let after_refusal = looked_up(&router, "EventName", "Encrypt").await;
    let (_, refused_record) = by_request_id("the refusal", &after_refusal, &refused.request_id);
    assert_eq!(
        refused_record.get("errorCode"),
        Some(&json!("NotFoundException")),
        "the trail records the fault shape the caller was answered with, got {refused_record}"
    );
    let unchanged = kms_ok(
        &router,
        "after the refusal",
        "GetKeyLastUsage",
        &json!({"KeyId": key_id}),
    )
    .await;
    assert_eq!(
        unchanged.body["KeyLastUsage"]["KmsRequestId"],
        json!(last_request_id),
        "a refused call used no key, so it recorded no use of one, got {}",
        unchanged.body
    );
}

/// The rule, continued, for the use of a key that is NOT a client calling KMS: a service
/// reaching KMS in-process for the caller it is serving — Amazon SQS asking for the data key
/// an encrypted queue's send needs — records a call of `kms.amazonaws.com` of its own, the
/// key's usage record names THAT call's request and THAT call's event, and the id it names
/// belongs to no other call: the next client call to KMS is answered under an id of its own.
#[tokio::test]
async fn a_key_used_by_another_services_in_process_call_is_recorded_as_that_call() {
    let router = router();

    let created = kms_ok(
        &router,
        "the queue's key",
        "CreateKey",
        &json!({"Description": "an encrypted queue's key"}),
    )
    .await;
    let key_id = created.body["KeyMetadata"]["KeyId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateKey answers a KeyId, got {}", created.body))
        .to_string();
    let key_arn = created.body["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateKey answers an Arn, got {}", created.body))
        .to_string();

    // A queue standing on that key: every send writes its body under a data key Amazon SQS
    // asks KMS for while serving the send.
    let queue = sqs_ok(
        &router,
        "the queue",
        "CreateQueue",
        &json!({
            "QueueName": "kms-audited-queue",
            "Attributes": {"KmsMasterKeyId": key_arn},
        }),
    )
    .await;
    let queue_url = queue.body["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateQueue answers a QueueUrl, got {}", queue.body))
        .to_string();
    let sent = sqs_ok(
        &router,
        "the send",
        "SendMessage",
        &json!({"QueueUrl": queue_url, "MessageBody": "under the queue's key"}),
    )
    .await;

    // The key's last use is the call AMAZON SQS made, and it names both of that call's ids.
    let usage = tracked_usage(
        &router,
        "the data key the send needed",
        &key_id,
        "GenerateDataKey",
        None,
    )
    .await;
    let service_call_request_id = usage["KmsRequestId"]
        .as_str()
        .unwrap_or_else(|| panic!("the record names the request of the call, got {usage}"))
        .to_string();
    let service_call_event_id = usage["CloudTrailEventId"]
        .as_str()
        .unwrap_or_else(|| panic!("the record names the event of the call, got {usage}"))
        .to_string();
    assert!(
        !service_call_request_id.is_empty() && !service_call_event_id.is_empty(),
        "a use another service's call made is named by both ids of that call, got {usage}"
    );
    assert_ne!(
        service_call_request_id, sent.request_id,
        "the record names the KMS call Amazon SQS made, not the send the client made, got \
         {usage}"
    );

    // And those ids are the record of that very call in event history, which names the
    // service that made it.
    let generated = looked_up(&router, "EventName", "GenerateDataKey").await;
    let (event, record) = by_request_id(
        "the data key the send needed",
        &generated,
        &service_call_request_id,
    );
    assert_eq!(
        event["EventId"].as_str(),
        Some(service_call_event_id.as_str()),
        "the usage record names the event CloudTrail recorded the call in, got {event} against \
         {usage}"
    );
    assert_eq!(
        record.get("eventSource"),
        Some(&json!(KMS_EVENT_SOURCE)),
        "the record belongs to the service that was CALLED, got {record}"
    );
    assert_eq!(
        record["userIdentity"]["invokedBy"],
        json!("sqs.amazonaws.com"),
        "the record names the service that made the call for the caller, got {record}"
    );

    // The receive serves the body back, so the in-process legs of the data plane are intact.
    let received = sqs_ok(
        &router,
        "the receive",
        "ReceiveMessage",
        &json!({"QueueUrl": queue_url, "MaxNumberOfMessages": 1}),
    )
    .await;
    assert_eq!(
        received.body["Messages"][0]["Body"],
        json!("under the queue's key"),
        "the message written under the queue's key is served back, got {}",
        received.body
    );

    // A CLIENT's own call to KMS afterwards is answered under an id of ITS own, and that is
    // the id its usage record carries: no id another call stated can name it.
    let encrypted = kms_ok(
        &router,
        "the client's own call",
        "Encrypt",
        &json!({
            "KeyId": key_id,
            "Plaintext": aws_smithy_types::base64::encode(PLAINTEXT),
        }),
    )
    .await;
    assert_ne!(
        encrypted.request_id, service_call_request_id,
        "a client's call is named by a request of its own, not by the one another service's \
         call stated"
    );
    let own_usage = tracked_usage(
        &router,
        "the client's own call",
        &key_id,
        "Encrypt",
        Some(&encrypted.request_id),
    )
    .await;
    assert_eq!(
        own_usage["KmsRequestId"],
        json!(encrypted.request_id),
        "the record of a client's call names that client's own request, got {own_usage}"
    );
    let own_event = looked_up(&router, "EventName", "Encrypt").await;
    let (_, own_record) = by_request_id(
        "the client's own call",
        &own_event,
        &encrypted.request_id,
    );
    assert_eq!(
        own_record["userIdentity"].get("invokedBy"),
        None,
        "a call the client made itself names no service that made it, got {own_record}"
    );
}
